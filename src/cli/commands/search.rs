//! `atlas search` command.

pub fn run(query: &str, project: &str, limit: usize) -> anyhow::Result<()> {
    println!("Searching '{}' in {} (limit: {})", query, project, limit);
    println!("(Stub -- full implementation in M5 Search & Context)");
    Ok(())
}
