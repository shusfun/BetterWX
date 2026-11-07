import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Copy, Download, FolderOpen, LoaderCircle, Pause, Play, ShieldCheck } from 'lucide-react';

type Location = { path: string; version: string };
type Status = Location & { enabled: boolean; supported: boolean; issue: string | null; outdated: boolean; backup: boolean; backupPath: string };
type Runtime = { version: string | null; compatible: boolean };
type AppInfo = { version: string; repository: string; runtime: Runtime; locations: Location[]; updateConfigured: boolean };
type Update = { version: string; notes: string };
type Operation = { operationId: string; action: string; status: 'running' | 'success' | 'error' | 'paused'; message: string; bytes: number; total: number; speed: number; eta: number | null; cancellable: boolean; records: string[]; result: { kind: string; status?: Status; update?: Update | null; runtime?: Runtime } | null };

const desktop = isTauri();
const speedText = (bytes: number) => `${(bytes / 1048576).toFixed(1)} MB/s`;

export default function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [path, setPath] = useState('');
  const [status, setStatus] = useState<Status | null>(null);
  const [task, setTask] = useState<Operation | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [update, setUpdate] = useState<Update | null>(null);
  const [downloaded, setDownloaded] = useState(false);
  const initialized = useRef(false);
  const automaticCheck = useRef(true);
  const lastResult = useRef('');

  const run = useCallback(async (command: string, args?: Record<string, unknown>) => {
    setError('');
    setBusy(true);
    try { await invoke(command, args); } catch (e) { setError(String(e)); void invoke('open_notice', { title: '操作未完成', message: String(e) }); setBusy(false); }
  }, []);

  const receive = useCallback((next: Operation) => {
    setTask(next);
    setBusy(next.status === 'running');
    if (next.status === 'running' || lastResult.current === next.operationId) return;
    lastResult.current = next.operationId;
    if (next.status === 'error' && next.action !== '检查应用更新') { setError(next.message); void invoke('open_notice', { title: '操作未完成', message: next.message }); }
    const result = next.result;
    if (result?.kind === 'scan' && result.status) { setStatus(result.status); setPath(result.status.path); }
    if (result?.kind === 'update') { setUpdate(result.update ?? null); setDownloaded(false); }
    if (result?.kind === 'downloaded') setDownloaded(true);
    if (result?.kind === 'runtime' && result.runtime) setInfo((old) => old ? { ...old, runtime: result.runtime! } : old);
    if (automaticCheck.current) { automaticCheck.current = false; setTimeout(() => { void run('check_update'); }, 300); }
  }, [run]);

  useEffect(() => {
    if (!desktop || initialized.current) return;
    initialized.current = true;
    let alive = true;
    const disposers: (() => void)[] = [];
    void (async () => {
      const offOperation = await listen<Operation>('operation', (event) => receive(event.payload));
      const offClose = await listen<string>('close-blocked', (event) => { setError(event.payload); void invoke('open_notice', { title: '操作进行中', message: event.payload }); });
      const offRuntime = await listen('update-runtime-requested', () => { void run('update_runtime'); });
      if (!alive) { offOperation(); offClose(); offRuntime(); return; }
      disposers.push(offOperation, offClose, offRuntime);
      await invoke('frontend_ready');
      const value = await invoke<AppInfo>('info');
      if (!alive) return;
      setInfo(value);
      const current = await invoke<Operation | null>('current_task');
      if (current) receive(current);
      else if (value.locations[0]) { setPath(value.locations[0].path); await run('scan', { path: value.locations[0].path }); }
      else { automaticCheck.current = false; await run('check_update'); }
    })().catch((e) => { setError(String(e)); void invoke('open_notice', { title: '启动失败', message: String(e) }); setBusy(false); });
    return () => { alive = false; disposers.forEach((off) => off()); initialized.current = false; };
  }, [receive, run]);

  async function pick() {
    try {
      const selected = await invoke<string | null>('pick_weixin');
      if (selected) { setPath(selected); setStatus(null); await run('scan', { path: selected }); }
    } catch (e) { setError(String(e)); void invoke('open_notice', { title: '无法选择微信', message: String(e) }); }
  }

  async function updateAction() {
    if (!update) return run('check_update');
    if (!downloaded) return run('download_update');
    return run('install_update');
  }

  async function openDiagnostics() {
    const diagnostics = [
        `BetterWX ${info?.version ?? ''}`, `微信 ${status?.version ?? 'unknown'} · ${path}`,
        `防撤回 ${status?.enabled ? '已开启' : '未开启'}`, `备份 ${status?.backupPath ?? ''}`, status?.issue ?? '',
        task ? `操作 ID: ${task.operationId}\n${task.action} · ${task.status}\n${task.message}\n${task.records.join('\n')}` : '', error,
      ].filter(Boolean).join('\n');
    await invoke('open_diagnostics', { data: {
      diagnostics,
      path,
      version: status?.version,
      enabled: status?.enabled,
      issue: status?.issue,
      operationId: task?.operationId,
      action: task?.action,
      status: task?.status,
      records: task?.records ?? [],
    } });
  }

  const enabled = status?.enabled ?? false;
  const running = task?.status === 'running';
  const downloading = running && task.total > 0;
  const supported = Boolean(status?.supported && path);
  const updateLabel = update ? (downloaded ? '重启并更新' : `下载 v${update.version}`) : '检查更新';

  return <div className="tool-window">
    <header className="tool-header">
      <div className="brand"><span className="brand-mark"><ShieldCheck size={18} /></span><strong>BetterWX</strong></div>
      <div className="wechat-state" title={path || '未选择微信'}><span className={`state-dot ${status?.supported ? 'ok' : ''}`} /><span>{status ? `微信 ${status.version}` : '检测微信中…'}</span></div>
      <span className="app-version">v{info?.version ?? '—'}</span>
    </header>
    <main className="tool-main">
      <div className="protection-row">
        <div className={`protection-icon ${enabled ? 'enabled' : ''}`}><ShieldCheck size={25} /></div>
        <div className="protection-label"><strong>防撤回</strong><span>{status?.issue ?? (enabled ? '已开启' : supported ? '未开启' : '等待检测')}</span></div>
        <button className="switch" role="switch" aria-label="防撤回" aria-checked={enabled} disabled={!desktop || busy || !supported} onClick={() => void run('set_revoke', { path, enabled: !enabled })}><span /></button>
      </div>
      {downloading && <div className="progress-row" aria-label="下载进度"><progress max={task.total} value={task.bytes} /><span>{Math.round(task.bytes / task.total * 100)}%</span><span className="speed">{speedText(task.speed)}</span><button className="mini-button" title="暂停下载" aria-label="暂停下载" onClick={() => void invoke('cancel')}><Pause size={14} /></button></div>}
      {task?.status === 'paused' && task.action.includes('下载') && <button className="resume-row" onClick={() => void run('download_update')}><Play size={13} />继续下载</button>}
      {!downloading && running && <div className="status-row"><LoaderCircle size={14} className="spin" />{task.message}</div>}
    </main>
    <footer className="tool-footer">
      <button className="tool-button" title="选择微信文件" aria-label="选择微信文件" disabled={!desktop || busy} onClick={() => void pick()}><FolderOpen size={18} /></button>
      <button className={`tool-button update-button ${update ? 'has-update' : ''}`} title={updateLabel} aria-label={updateLabel} disabled={!desktop || busy} onClick={() => void updateAction()}>{busy && task?.action.includes('更新') ? <LoaderCircle size={18} className="spin" /> : <Download size={18} />}</button>
      <button className="tool-button" title="诊断信息" aria-label="诊断信息" onClick={() => void openDiagnostics()}><Copy size={18} /></button>
      <span className="footer-spacer" />
    </footer>
    {!desktop && <div className="preview-note">请在桌面版中使用</div>}
  </div>;
}
