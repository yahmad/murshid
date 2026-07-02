# Design Plan: Goal/Task Tracking System for Murshid

This document outlines the architectural and design specifications for integrating a **Goal/Task tracking system** into Murshid. The primary purpose of this system is to maintain project-level context for the Socratic AI mentor, ensuring that hints and guidance are aligned with the developer's immediate objectives.

---

## 1. Objectives

1.  **Objective Alignment:** Allow the developer to set project-level goals so that the Socratic AI mentor understands the context of compile errors in relation to the active task.
2.  **CLI Goal Management:** Provide a clean command-line interface (`murshid goal`) to set, retrieve, complete, and list goals.
3.  **Local SQLite Storage:** Persist goals locally inside Murshid's existing SQLite profile database using a robust database migration.
4.  **Prompt Injection:** Seamlessly inject the active goal as structural XML tags in LLM prompts dispatched during compiler error interceptions.

---

## 2. Database Schema Changes & Migrations

We will introduce a new migration inside [db.rs](file:///Users/yasir/src/yahmad/murshid/src/db.rs) to bump the `PRAGMA user_version` from `1` to `2`.

### 2.1 The `goals` Table Schema
We will create a new `goals` table to store goals:

```sql
CREATE TABLE IF NOT EXISTS goals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL CHECK(status IN ('active', 'completed', 'abandoned')),
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    completed_at TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);
```

### 2.2 Database Migration Code (`src/db.rs`)
In `run_migrations`, we will check if `current_version < 2` and execute the schema creation statements inside a database transaction:

```rust
if current_version < 2 {
    let tx = conn.transaction()?;

    tx.execute(
        "CREATE TABLE IF NOT EXISTS goals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            description TEXT,
            status TEXT NOT NULL CHECK(status IN ('active', 'completed', 'abandoned')),
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            completed_at TIMESTAMP
        );",
        [],
    )?;

    tx.execute(
        "CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);",
        [],
    )?;

    tx.execute("PRAGMA user_version = 2;", [])?;
    tx.commit()?;
    current_version = 2;
}
```

---

## 3. CLI Commands Interface (`murshid goal`)

We will introduce a new CLI subcommand suite matching the pattern `murshid goal <subcommand>`.

### 3.1 Subcommand List

| Subcommand | Usage | Description |
|---|---|---|
| **`set`** | `murshid goal set "<title>" ["<description>"]` | Creates a new goal and marks it as `active`. To ensure exactly one active goal at a time, setting a new active goal will automatically mark any existing `active` goal as `abandoned` or `completed`. |
| **`get`** | `murshid goal get` | Displays the title, description, elapsed time, and status of the currently active goal. |
| **`complete`** | `murshid goal complete` | Marks the currently active goal as `completed` and sets `completed_at` to the current timestamp. |
| **`list`** | `murshid goal list` | Lists all goals tracked in the repository in a tabular format. |

### 3.2 CLI Examples & Help Output

```text
Usage:
  murshid goal set "<title>" ["<description>"]
  murshid goal get
  murshid goal complete
  murshid goal list
```

### 3.3 Implementation of CLI Subcommand (`src/cli/goal.rs`)
We will create a new CLI module file `src/cli/goal.rs` implementing functions to interact with the database:

-   `set_active_goal(conn: &Connection, title: &str, description: Option<&str>)`
    Marks any existing `'active'` goals as `'abandoned'` and inserts the new active goal.
-   `get_active_goal(conn: &Connection) -> Result<Option<Goal>>`
    Fetches the active goal row.
-   `complete_active_goal(conn: &Connection)`
    Updates the active goal status to `'completed'` and sets `completed_at = CURRENT_TIMESTAMP`.
-   `list_goals(conn: &Connection)`
    Fetches all goals, formatting and printing them to stdout.

---

## 4. LLM Prompt Injection Architecture

To ensure the AI mentor has access to the user's active goal when guiding them through compiler errors, we will inject the goal into the prompt payload.

### 4.1 XML Representation
When an active goal is present in the database, it will be represented in the prompt payload using the following structure:

```xml
<active_goal>
  <title>Active Goal Title</title>
  <description>Optional description of the active goal...</description>
</active_goal>
```

### 4.2 Injection Points

1.  **File Watcher Loop (`src/main.rs`)**:
    Inside the watcher code check response block, when we retrieve API keys and dispatch the compiler check diagnostic to the provider, we will:
    -   Open a connection to the SQLite database.
    -   Fetch the currently active goal.
    -   If an active goal exists, format the `<active_goal>` XML payload.
    -   Prepend or wrap the `context_payload` alongside the `<active_goal>` tag, creating a unified payload.

```rust
// In main.rs (watch loop)
let active_goal_payload = if let Ok(conn) = db::open_connection(&db_path) {
    match cli_goal::get_active_goal(&conn) {
        Ok(Some(goal)) => {
            let desc = goal.description.unwrap_or_default();
            format!(
                "<active_goal>\n  <title>{}</title>\n  <description>{}</description>\n</active_goal>\n",
                context::sanitize_xml(&goal.title),
                context::sanitize_xml(&desc)
            )
        }
        _ => String::new(),
    }
} else {
    String::new()
};

let full_prompt = format!("{}{}", active_goal_payload, context_payload);
match provider::dispatch_debounced(provider_type, &full_prompt, Some(&key)) {
    // ...
}
```

This injection is clean and avoids coupling [context.rs](file:///Users/yasir/src/yahmad/murshid/src/context.rs) (which handles file parsing and type signatures) with SQLite dependencies.

---

## 5. Implementation Steps

1.  **Database Migration:** Add schema definitions and bump the migration version inside `src/db.rs`.
2.  **Goal Module:** Implement `src/cli/goal.rs` to handle goal queries and format output.
3.  **CLI Registration:** Update command matching and help banners inside `src/main.rs` to register `murshid goal` subcommands.
4.  **Prompt Dispatch Update:** Retrieve the active goal and prepend its XML structure to the compiler check prompt context in `src/main.rs`.
5.  **Validation & Tests:** Add unit tests verifying database migration safety, active goal toggles, and formatting properties.
