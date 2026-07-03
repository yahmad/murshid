#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Goal {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub created_at: String,
    pub completed_at: Option<String>,
}

pub fn set_active_goal(
    conn: &rusqlite::Connection,
    title: &str,
    description: Option<&str>,
) -> Result<(), rusqlite::Error> {
    // Automatically abandon any existing active goals
    conn.execute(
        "UPDATE goals SET status = 'abandoned', completed_at = CURRENT_TIMESTAMP WHERE status = 'active';",
        [],
    )?;

    conn.execute(
        "INSERT INTO goals (title, description, status) VALUES (?1, ?2, 'active');",
        rusqlite::params![title, description],
    )?;

    Ok(())
}

pub fn get_active_goal(conn: &rusqlite::Connection) -> Result<Option<Goal>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description, status, created_at, completed_at FROM goals WHERE status = 'active' LIMIT 1;"
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        Ok(Some(Goal {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            status: row.get(3)?,
            created_at: row.get(4)?,
            completed_at: row.get(5)?,
        }))
    } else {
        Ok(None)
    }
}

pub fn complete_active_goal(conn: &rusqlite::Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        "UPDATE goals SET status = 'completed', completed_at = CURRENT_TIMESTAMP WHERE status = 'active';",
        [],
    )?;
    Ok(())
}

pub fn list_goals(conn: &rusqlite::Connection) -> Result<Vec<Goal>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, title, description, status, created_at, completed_at FROM goals ORDER BY created_at ASC;"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Goal {
            id: row.get(0)?,
            title: row.get(1)?,
            description: row.get(2)?,
            status: row.get(3)?,
            created_at: row.get(4)?,
            completed_at: row.get(5)?,
        })
    })?;
    let mut goals = Vec::new();
    for goal in rows {
        goals.push(goal?);
    }
    Ok(goals)
}

pub fn run_goal_cli(conn: &rusqlite::Connection, args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err(
            "Missing goal subcommand. Use 'set', 'get', 'complete', or 'list'.".to_string(),
        );
    }
    match args[0].as_str() {
        "set" => {
            if args.len() < 2 {
                return Err("Usage: murshid goal set <title> [<description>]".to_string());
            }
            let title = &args[1];
            let description = if args.len() > 2 {
                Some(args[2].as_str())
            } else {
                None
            };
            set_active_goal(conn, title, description).map_err(|e| e.to_string())?;
            println!("Active goal set successfully: {}", title);
            Ok(())
        }
        "get" => {
            match get_active_goal(conn).map_err(|e| e.to_string())? {
                Some(goal) => {
                    println!("Active Goal [ID: {}]:", goal.id);
                    println!("  Title:       {}", goal.title);
                    if let Some(ref desc) = goal.description {
                        println!("  Description: {}", desc);
                    }
                    println!("  Status:      {}", goal.status);
                    println!("  Created At:  {}", goal.created_at);
                }
                None => {
                    println!("No active goal set. Use 'murshid goal set <title>' to start a task.");
                }
            }
            Ok(())
        }
        "complete" => {
            match get_active_goal(conn).map_err(|e| e.to_string())? {
                Some(goal) => {
                    complete_active_goal(conn).map_err(|e| e.to_string())?;
                    println!("Goal completed successfully: {}", goal.title);
                }
                None => {
                    println!("No active goal to complete.");
                }
            }
            Ok(())
        }
        "list" => {
            let goals = list_goals(conn).map_err(|e| e.to_string())?;
            if goals.is_empty() {
                println!("No goals tracked yet.");
                return Ok(());
            }
            println!(
                "{:<5} | {:<25} | {:<10} | {:<20} | {:<20}",
                "ID", "Title", "Status", "Created At", "Completed At"
            );
            println!("{:-<90}", "");
            for g in goals {
                let completed = g.completed_at.as_deref().unwrap_or("-");
                let title_trunc = if g.title.len() > 23 {
                    format!("{}...", &g.title[..20])
                } else {
                    g.title.clone()
                };
                println!(
                    "{:<5} | {:<25} | {:<10} | {:<20} | {:<20}",
                    g.id, title_trunc, g.status, g.created_at, completed
                );
            }
            Ok(())
        }
        _ => Err(format!(
            "Unknown goal subcommand: '{}'. Use 'set', 'get', 'complete', or 'list'.",
            args[0]
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::initialize_db;

    #[test]
    fn test_goal_tracking_lifecycle() {
        let conn = initialize_db(":memory:").unwrap();

        // 1. Initial get should return None
        let active = get_active_goal(&conn).unwrap();
        assert!(active.is_none());

        // 2. Set active goal
        set_active_goal(&conn, "Write compiler", Some("Rust compiler check")).unwrap();
        let active = get_active_goal(&conn).unwrap().unwrap();
        assert_eq!(active.title, "Write compiler");
        assert_eq!(active.description.as_deref(), Some("Rust compiler check"));
        assert_eq!(active.status, "active");
        assert!(active.completed_at.is_none());

        // 3. Set another active goal (should abandon previous)
        set_active_goal(&conn, "Write DB test", None).unwrap();
        let active = get_active_goal(&conn).unwrap().unwrap();
        assert_eq!(active.title, "Write DB test");
        assert_eq!(active.status, "active");

        // 4. Verify previous goal status was updated to abandoned
        let goals = list_goals(&conn).unwrap();
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[0].title, "Write compiler");
        assert_eq!(goals[0].status, "abandoned");
        assert!(goals[0].completed_at.is_some());
        assert_eq!(goals[1].title, "Write DB test");
        assert_eq!(goals[1].status, "active");

        // 5. Complete active goal
        complete_active_goal(&conn).unwrap();
        let active = get_active_goal(&conn).unwrap();
        assert!(active.is_none());

        // 6. Verify completion persists
        let goals = list_goals(&conn).unwrap();
        assert_eq!(goals[1].status, "completed");
        assert!(goals[1].completed_at.is_some());
    }
}
