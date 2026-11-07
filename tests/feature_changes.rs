use betterwx::{
    feature::{self, Edit, Mode, Prepared},
    progress::Progress,
    store::Store,
};
use std::{collections::BTreeMap, path::PathBuf};

fn prepared(path: PathBuf) -> Vec<Prepared> {
    [(&feature::FEATURES[2], 8u8), (&feature::FEATURES[0], 16u8)]
        .into_iter()
        .map(|(f, offset)| Prepared {
            feature: f,
            edits: Ok(vec![Mode {
                id: "enabled",
                name: "已启用",
                edits: BTreeMap::from([(
                    path.clone(),
                    vec![Edit {
                        range: offset as usize..offset as usize + 1,
                        bytes: vec![offset],
                    }],
                )]),
            }]),
            resources: BTreeMap::new(),
        })
        .collect()
}

#[test]
fn toggling_revoke_keeps_other_feature_and_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Weixin.dll");
    std::fs::write(&path, [0u8; 32]).unwrap();
    let progress = Progress::default();
    let mut store = Store::open(&path, &progress).unwrap();
    let path = store.target.clone();
    store.files.get_mut(&path).unwrap().source_issue = None;
    let p = prepared(path.clone());
    for (index, mode) in [(1, Some(0)), (0, Some(0)), (0, Some(0)), (0, None)] {
        feature::inspect(&mut store, &p, &progress).unwrap();
        let content = feature::change(&store, &p[index], mode, &p, &progress).unwrap();
        store.save(content, &progress).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes[8], 0);
    assert_eq!(bytes[16], 16);
    let backup = Store::backup_path(&path).unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), [0u8; 32]);
    assert!(
        backup.starts_with(
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join("BetterWX")
                .join("backups")
        )
    );
    assert!(!path.with_added_extension("bak").exists());
}

#[test]
fn legacy_install_backup_is_read_but_new_backup_is_external() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Weixin.dll");
    let original = [0u8; 32];
    std::fs::write(&path, original).unwrap();
    std::fs::write(path.with_added_extension("bak"), original).unwrap();
    let progress = Progress::default();
    let mut store = Store::open(&path, &progress).unwrap();
    let path = store.target.clone();
    store.files.get_mut(&store.target).unwrap().source_issue = None;
    let p = prepared(store.target.clone());
    feature::inspect(&mut store, &p, &progress).unwrap();
    let contents = feature::change(&store, &p[0], Some(0), &p, &progress).unwrap();
    store.save(contents, &progress).unwrap();
    let backup = Store::backup_path(&path).unwrap();
    assert_eq!(std::fs::read(backup).unwrap(), original);
    assert_eq!(
        std::fs::read(path.with_added_extension("bak")).unwrap(),
        original
    );
}

#[test]
fn changed_target_and_untrusted_source_cannot_be_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Weixin.dll");
    std::fs::write(&path, [0u8; 32]).unwrap();
    let progress = Progress::default();
    let mut store = Store::open(&path, &progress).unwrap();
    let p = prepared(store.target.clone());
    assert!(feature::change(&store, &p[0], Some(0), &p, &progress).is_err());
    store.files.get_mut(&store.target).unwrap().source_issue = None;
    let contents = feature::change(&store, &p[0], Some(0), &p, &progress).unwrap();
    std::fs::write(&path, [9u8; 32]).unwrap();
    assert!(store.save(contents, &progress).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), [9u8; 32]);
}
