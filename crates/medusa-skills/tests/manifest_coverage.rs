use medusa_skills::SkillIndex;

#[test]
fn empty_assets_directory_produces_empty_index() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    let index = SkillIndex::from_assets_dir(dir.path()).unwrap();
    assert_eq!(index.entries().len(), 0);
}
