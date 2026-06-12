//! Integration tests for xaft-skills.

use tempfile::TempDir;
use xaft_skills::{Skill, SkillLoader};

async fn write_file(dir: &std::path::Path, name: &str, content: &str) {
    tokio::fs::write(dir.join(name), content).await.unwrap();
}

#[tokio::test]
async fn loader_loads_skill_from_dir() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();
    write_file(
        &skills_dir,
        "test.md",
        "---\nname: test\ndescription: A test skill\n---\nDo the thing.\n",
    )
    .await;

    let loader = SkillLoader::new(vec![skills_dir]);
    let skills = loader.load_all().await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "test");
    assert!(skills[0].body.contains("Do the thing."));
}

#[tokio::test]
async fn loader_parses_frontmatter_name_and_description() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();
    write_file(
        &skills_dir,
        "skill.md",
        "---\nname: my-skill\ndescription: Does X\n---\nContent here.\n",
    )
    .await;

    let loader = SkillLoader::new(vec![skills_dir]);
    let skills = loader.load_all().await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "my-skill");
    assert_eq!(skills[0].description, "Does X");
}

#[tokio::test]
async fn loader_loads_skill_bundle_from_skill_md() {
    let tmp = TempDir::new().unwrap();
    let working_dir = tmp.path();
    let skills_dir = working_dir.join(".xaft").join("skills");
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();

    let bundle = "## Skill: alpha\nDo alpha things.\n## Skill: beta\nDo beta things.\n";
    write_file(working_dir, "SKILL.md", bundle).await;

    let loader = SkillLoader::new(vec![skills_dir]);
    let skills = loader.load_all().await;
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"alpha"), "expected alpha in {names:?}");
    assert!(names.contains(&"beta"), "expected beta in {names:?}");
    let alpha = skills.iter().find(|s| s.name == "alpha").unwrap();
    assert!(alpha.body.contains("alpha things"));
}

#[tokio::test]
async fn loader_respects_size_limit() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();

    // Write a file slightly larger than MAX_SKILL_BYTES (32 768 bytes).
    let big = "Y".repeat(33_000);
    write_file(&skills_dir, "big.md", &big).await;

    let loader = SkillLoader::new(vec![skills_dir]);
    let skills = loader.load_all().await;
    assert_eq!(skills.len(), 1, "skill should be loaded despite size");
    // Body must fit within per-file cap.
    assert!(
        skills[0].body.len() <= 32_768,
        "body exceeds per-file cap: {}",
        skills[0].body.len()
    );
}

#[tokio::test]
async fn loader_deduplicates_by_name_local_wins() {
    let tmp = TempDir::new().unwrap();
    let local_dir = tmp.path().join("local").join("skills");
    let global_dir = tmp.path().join("global").join("skills");
    tokio::fs::create_dir_all(&local_dir).await.unwrap();
    tokio::fs::create_dir_all(&global_dir).await.unwrap();

    write_file(
        &local_dir,
        "shared.md",
        "---\nname: shared\n---\nlocal body\n",
    )
    .await;
    write_file(
        &global_dir,
        "shared.md",
        "---\nname: shared\n---\nglobal body\n",
    )
    .await;

    let loader = SkillLoader::new(vec![local_dir, global_dir]);
    let skills = loader.load_all().await;
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].body.trim(), "local body");
}

#[tokio::test]
async fn loader_sorts_by_priority() {
    let tmp = TempDir::new().unwrap();
    let skills_dir = tmp.path().join("skills");
    tokio::fs::create_dir_all(&skills_dir).await.unwrap();

    write_file(
        &skills_dir,
        "low.md",
        "---\nname: low\npriority: 1\n---\nlow priority skill",
    )
    .await;
    write_file(
        &skills_dir,
        "high.md",
        "---\nname: high\npriority: 10\n---\nhigh priority skill",
    )
    .await;
    write_file(
        &skills_dir,
        "mid.md",
        "---\nname: mid\npriority: 5\n---\nmid priority skill",
    )
    .await;

    let loader = SkillLoader::new(vec![skills_dir]);
    let skills = loader.load_all().await;
    assert_eq!(skills.len(), 3);
    assert_eq!(skills[0].name, "high");
    assert_eq!(skills[1].name, "mid");
    assert_eq!(skills[2].name, "low");
}

#[tokio::test]
async fn build_prompt_section_empty_for_no_skills() {
    let section = SkillLoader::build_prompt_section(&[]);
    assert_eq!(section, "");
}

#[tokio::test]
async fn build_prompt_section_includes_all_skills() {
    let skills = vec![
        Skill {
            name: "skill-a".into(),
            description: "desc-a".into(),
            tags: vec![],
            body: "body-a".into(),
            source_path: "/tmp/a.md".into(),
            priority: 0,
        },
        Skill {
            name: "skill-b".into(),
            description: "desc-b".into(),
            tags: vec![],
            body: "body-b".into(),
            source_path: "/tmp/b.md".into(),
            priority: 0,
        },
    ];
    let section = SkillLoader::build_prompt_section(&skills);
    assert!(section.contains("# Loaded Skills"));
    assert!(section.contains("Skill: skill-a"));
    assert!(section.contains("Skill: skill-b"));
    assert!(section.contains("body-a"));
    assert!(section.contains("body-b"));
}
