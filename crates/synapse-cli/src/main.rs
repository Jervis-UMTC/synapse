use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use synapse_core::knowledge::{
    Confidence, KnowledgeRecord, KnowledgeRelation, KnowledgeRelationKind, KnowledgeState,
    Provenance, ProvenanceBasis,
};
use synapse_ipc::{
    request as ipc_request, serve, serve_once, trust_executable, IpcRequest, IpcValue,
};
use synapse_store::{
    AuthorizationPolicy, ClientAuthorization, FileStore, KnowledgeHit, KnowledgeQuery,
};

fn main() {
    if let Err(error) = run(std::env::args().skip(1).collect()) {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("--version") {
        println!("synapse {}", synapse_core::VERSION);
        return Ok(());
    }

    if args.first().map(String::as_str) == Some("doctor") {
        if args.len() != 1 {
            return Err(usage());
        }
        return doctor();
    }

    if args.first().map(String::as_str) == Some("authorization") {
        return match args.get(1).map(String::as_str) {
            Some("init") => initialize_authorization(&args[2..]),
            _ => Err(usage()),
        };
    }

    if args.first().map(String::as_str) == Some("ipc") {
        return manage_ipc(&args[1..]);
    }

    if args.first().map(String::as_str) == Some("knowledge") {
        return match args.get(1).map(String::as_str) {
            Some("create") => create_knowledge(&args[2..]),
            Some("add") => add_knowledge(&args[2..]),
            Some("replace") => replace_knowledge(&args[2..]),
            Some("show") => show_knowledge(&args[2..]),
            Some("find") => find_knowledge(&args[2..]),
            Some("relate") => relate_knowledge(&args[2..]),
            Some("status") => status_knowledge(&args[2..]),
            Some("index") => manage_knowledge_index(&args[2..]),
            _ => Err(usage()),
        };
    }

    Err(usage())
}

fn doctor() -> Result<(), String> {
    let root = store_root()?;
    let inspection = FileStore::new(&root)
        .inspect()
        .map_err(|error| format!("Synapse doctor failed: {error}"))?;

    println!("Synapse doctor");
    if !inspection.root_exists {
        println!("Store root: INFO not initialized ({})", root.display());
        println!("Authorization: INFO not configured");
        println!("Records: OK 0");
        println!("Relations: OK 0");
        println!("Index: INFO not built");
        println!("Store identity: INFO not initialized");
        println!("Overall: OK (no store initialized)");
        return Ok(());
    }

    println!("Store root: OK {}", root.display());
    match inspection.authorization_clients {
        Some(1) => println!("Authorization: OK 1 client"),
        Some(clients) => println!("Authorization: OK {clients} clients"),
        None => println!("Authorization: INFO not configured"),
    }
    println!("Records: OK {}", inspection.record_count);
    println!("Relations: OK {}", inspection.relation_count);
    match inspection.index_entry_count {
        Some(1) => println!("Index: OK ready (1 entry)"),
        Some(entries) => println!("Index: OK ready ({entries} entries)"),
        None => println!("Index: INFO not built"),
    }
    match inspection.store_id {
        Some(id) => println!("Store identity: OK {id}"),
        None => println!("Store identity: INFO not initialized"),
    }
    println!("Overall: OK");
    Ok(())
}

fn initialize_authorization(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err(usage());
    }

    let owner = ClientAuthorization::new(&args[0], true, true)
        .map_err(|error| format!("invalid authorization owner: {error}"))?;
    let policy = AuthorizationPolicy::new(vec![owner])
        .map_err(|error| format!("invalid authorization policy: {error}"))?;
    FileStore::new(store_root()?)
        .initialize_authorization(&policy)
        .map_err(|error| error.to_string())?;
    let json = serde_json::to_string_pretty(&policy)
        .map_err(|error| format!("failed to serialize authorization policy: {error}"))?;
    println!("{json}");
    Ok(())
}

fn manage_ipc(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("serve") => {
            let once = match args.get(1).map(String::as_str) {
                None => false,
                Some("--once") if args.len() == 2 => true,
                _ => return Err(usage()),
            };
            let store = FileStore::new(store_root()?);
            if once {
                serve_once(store).map_err(|error| error.to_string())
            } else {
                serve(store).map_err(|error| error.to_string())
            }
        }
        Some("trust") if args.len() == 3 => {
            let store = FileStore::new(store_root()?);
            let trusted = trust_executable(&store, &args[1], &PathBuf::from(&args[2]))
                .map_err(|error| error.to_string())?;
            let json = serde_json::to_string_pretty(&trusted)
                .map_err(|error| format!("failed to serialize IPC trust entry: {error}"))?;
            println!("{json}");
            Ok(())
        }
        Some("ping") if args.len() == 1 => match ipc_call(IpcRequest::Ping)? {
            IpcValue::Pong => {
                println!("pong");
                Ok(())
            }
            _ => Err("Synapse IPC returned an unexpected ping response".to_owned()),
        },
        _ => Err(usage()),
    }
}

fn create_knowledge(args: &[String]) -> Result<(), String> {
    let record = build_knowledge_record(args)?;
    print_record(&record)
}

fn add_knowledge(args: &[String]) -> Result<(), String> {
    let record = build_knowledge_record(args)?;
    if ipc_enabled() {
        return match ipc_call(IpcRequest::InsertRecord { record })? {
            IpcValue::InsertedRecord { record } => print_record(&record),
            _ => Err("Synapse IPC returned an unexpected record insertion response".to_owned()),
        };
    }

    let store = FileStore::new(store_root()?);
    store.insert(&record).map_err(|error| error.to_string())?;
    print_record(&record)
}

fn replace_knowledge(args: &[String]) -> Result<(), String> {
    if args.len() < 8 {
        return Err(usage());
    }

    let record = apply_record_address_options(build_knowledge_record(&args[..6])?, &args[8..])?;
    let relation = KnowledgeRelation::new(
        &args[6],
        &record.id,
        KnowledgeRelationKind::Supersedes,
        &args[7],
        &record.source,
        current_unix_ms()?,
        Provenance {
            basis: record.provenance.basis,
            detail: None,
        },
    )
    .map_err(|error| error.to_string())?;

    let (record, relation) = if ipc_enabled() {
        match ipc_call(IpcRequest::InsertSuccessor { record, relation })? {
            IpcValue::InsertedSuccessor { record, relation } => (record, relation),
            _ => {
                return Err(
                    "Synapse IPC returned an unexpected atomic successor response".to_owned(),
                )
            }
        }
    } else {
        FileStore::new(store_root()?)
            .insert_successor(&record, &relation)
            .map_err(|error| error.to_string())?;
        (record, relation)
    };

    let output = serde_json::json!({ "record": record, "relation": relation });
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .map_err(|error| format!("failed to serialize atomic successor: {error}"))?
    );
    Ok(())
}

fn show_knowledge(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err(usage());
    }

    let id = &args[0];
    let record = if ipc_enabled() {
        match ipc_call(IpcRequest::Get { id: id.clone() })? {
            IpcValue::Record { record } => record,
            _ => return Err("Synapse IPC returned an unexpected record response".to_owned()),
        }
    } else {
        FileStore::new(store_root()?)
            .get(id)
            .map_err(|error| error.to_string())?
    }
    .ok_or_else(|| format!("knowledge record '{id}' not found"))?;
    print_record(&record)
}

fn find_knowledge(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err(usage());
    }

    let mut query = KnowledgeQuery::default();
    let mut has_constraint = false;
    let mut index = 0usize;
    if !args[0].starts_with("--") {
        if args[0].trim().is_empty() {
            return Err(usage());
        }
        query.text = Some(args[0].clone());
        has_constraint = true;
        index = 1;
    }

    while index < args.len() {
        let option = args[index].as_str();
        let value = args.get(index + 1).ok_or_else(usage)?;
        if value.trim().is_empty() {
            return Err(format!("find option '{option}' must not be empty"));
        }
        match option {
            "--kind" => query.kind = Some(value.clone()),
            "--source" => query.source = Some(value.clone()),
            "--scope" => query.scope = Some(value.clone()),
            "--key" => query.key = Some(value.clone()),
            "--state" => query.state = parse_query_state(value)?,
            "--basis" => query.provenance_basis = parse_query_basis(value)?,
            "--limit" => {
                query.limit = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid query limit '{value}'"))?;
            }
            other => return Err(format!("unknown find option '{other}'\n{}", usage())),
        }
        has_constraint = true;
        index += 2;
    }

    if !has_constraint {
        return Err(usage());
    }

    let records = if ipc_enabled() {
        match ipc_call(IpcRequest::Query { query })? {
            IpcValue::Hits { hits } => hits,
            _ => return Err("Synapse IPC returned an unexpected query response".to_owned()),
        }
    } else {
        FileStore::new(store_root()?)
            .query(&query)
            .map_err(|error| error.to_string())?
    };
    print_records(&records)
}

fn relate_knowledge(args: &[String]) -> Result<(), String> {
    if args.len() != 6 {
        return Err(usage());
    }

    let kind = match args[2].as_str() {
        "supersedes" => KnowledgeRelationKind::Supersedes,
        "conflicts" | "conflicts_with" => KnowledgeRelationKind::ConflictsWith,
        other => {
            return Err(format!(
                "invalid knowledge relation '{other}'; use supersedes or conflicts"
            ))
        }
    };
    let basis = parse_required_basis(&args[5])?;
    let relation = KnowledgeRelation::new(
        &args[0],
        &args[1],
        kind,
        &args[3],
        &args[4],
        current_unix_ms()?,
        Provenance {
            basis,
            detail: None,
        },
    )
    .map_err(|error| error.to_string())?;

    let relation = if ipc_enabled() {
        match ipc_call(IpcRequest::InsertRelation { relation })? {
            IpcValue::InsertedRelation { relation } => relation,
            _ => {
                return Err(
                    "Synapse IPC returned an unexpected relation insertion response".to_owned(),
                )
            }
        }
    } else {
        FileStore::new(store_root()?)
            .insert_relation(&relation)
            .map_err(|error| error.to_string())?;
        relation
    };
    let json = serde_json::to_string_pretty(&relation)
        .map_err(|error| format!("failed to serialize knowledge relation: {error}"))?;
    println!("{json}");
    Ok(())
}

fn status_knowledge(args: &[String]) -> Result<(), String> {
    if args.len() != 1 {
        return Err(usage());
    }

    let id = &args[0];
    let status = if ipc_enabled() {
        match ipc_call(IpcRequest::Status { id: id.clone() })? {
            IpcValue::Status { status } => status,
            _ => return Err("Synapse IPC returned an unexpected status response".to_owned()),
        }
    } else {
        FileStore::new(store_root()?)
            .status(id)
            .map_err(|error| error.to_string())?
    }
    .ok_or_else(|| format!("knowledge record '{id}' not found"))?;
    let json = serde_json::to_string_pretty(&status)
        .map_err(|error| format!("failed to serialize knowledge status: {error}"))?;
    println!("{json}");
    Ok(())
}

fn manage_knowledge_index(args: &[String]) -> Result<(), String> {
    if args.len() != 1 || args[0] != "rebuild" {
        return Err(usage());
    }

    let indexed_records = FileStore::new(store_root()?)
        .rebuild_index()
        .map_err(|error| error.to_string())?;
    let output = serde_json::json!({ "indexed_records": indexed_records });
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .map_err(|error| format!("failed to serialize index rebuild result: {error}"))?
    );
    Ok(())
}

fn parse_query_state(value: &str) -> Result<Option<KnowledgeState>, String> {
    match value {
        "active" => Ok(Some(KnowledgeState::Active)),
        "stale" => Ok(Some(KnowledgeState::Stale)),
        "conflicted" => Ok(Some(KnowledgeState::Conflicted)),
        "superseded" => Ok(Some(KnowledgeState::Superseded)),
        "unknown" => Ok(Some(KnowledgeState::Unknown)),
        "any" => Ok(None),
        other => Err(format!(
            "invalid knowledge state '{other}'; use active, stale, conflicted, superseded, unknown, or any"
        )),
    }
}

fn parse_query_basis(value: &str) -> Result<Option<ProvenanceBasis>, String> {
    match value {
        "observed" => Ok(Some(ProvenanceBasis::Observed)),
        "inferred" => Ok(Some(ProvenanceBasis::Inferred)),
        "any" => Ok(None),
        other => Err(format!(
            "invalid provenance basis '{other}'; use observed, inferred, or any"
        )),
    }
}

fn build_knowledge_record(args: &[String]) -> Result<KnowledgeRecord, String> {
    if args.len() < 6 {
        return Err(usage());
    }

    let basis = parse_required_basis(&args[3])?;

    let confidence = match args[4].as_str() {
        "unknown" => Confidence::Unknown,
        "low" => Confidence::Low,
        "medium" => Confidence::Medium,
        "high" => Confidence::High,
        other => {
            return Err(format!(
                "invalid confidence '{other}'; use unknown, low, medium, or high"
            ))
        }
    };

    let created_at_unix_ms = current_unix_ms()?;
    let record = KnowledgeRecord::new(
        &args[0],
        &args[5],
        &args[1],
        &args[2],
        created_at_unix_ms,
        confidence,
        KnowledgeState::Active,
        Provenance {
            basis,
            detail: None,
        },
    )
    .map_err(|error| error.to_string())?;

    apply_record_address_options(record, &args[6..])
}

fn apply_record_address_options(
    record: KnowledgeRecord,
    args: &[String],
) -> Result<KnowledgeRecord, String> {
    if args.is_empty() {
        return Ok(record);
    }
    if args.len() % 2 != 0 {
        return Err(usage());
    }

    let mut scope = None;
    let mut key = None;
    let mut index = 0usize;
    while index < args.len() {
        let option = args[index].as_str();
        let value = args.get(index + 1).ok_or_else(usage)?;
        match option {
            "--scope" if scope.is_none() => scope = Some(value.clone()),
            "--key" if key.is_none() => key = Some(value.clone()),
            "--scope" | "--key" => {
                return Err(format!("duplicate record address option '{option}'"))
            }
            other => return Err(format!("unknown record option '{other}'\n{}", usage())),
        }
        index += 2;
    }

    match (scope, key) {
        (None, None) => Ok(record),
        (Some(scope), Some(key)) => record
            .with_address(scope, key)
            .map_err(|error| error.to_string()),
        (Some(_), None) => Err("--scope requires --key".to_owned()),
        (None, Some(_)) => Err("--key requires --scope".to_owned()),
    }
}

fn print_record(record: &KnowledgeRecord) -> Result<(), String> {
    let json = serde_json::to_string_pretty(record)
        .map_err(|error| format!("failed to serialize knowledge record: {error}"))?;
    println!("{json}");
    Ok(())
}

fn print_records(records: &[KnowledgeHit]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(records)
        .map_err(|error| format!("failed to serialize knowledge records: {error}"))?;
    println!("{json}");
    Ok(())
}

fn parse_required_basis(value: &str) -> Result<ProvenanceBasis, String> {
    match value {
        "observed" => Ok(ProvenanceBasis::Observed),
        "inferred" => Ok(ProvenanceBasis::Inferred),
        other => Err(format!(
            "invalid provenance basis '{other}'; use observed or inferred"
        )),
    }
}

fn current_unix_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis()
        .try_into()
        .map_err(|_| "current timestamp does not fit into u64 milliseconds".to_owned())
}

fn ipc_enabled() -> bool {
    std::env::var("SYNAPSE_IPC")
        .ok()
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

fn ipc_call(request: IpcRequest) -> Result<IpcValue, String> {
    let root = store_root()?;
    ipc_request(&root, &request).map_err(|error| error.to_string())
}

fn store_root() -> Result<PathBuf, String> {
    if let Some(path) = nonempty_env_path("SYNAPSE_STORE") {
        return Ok(path);
    }

    #[cfg(windows)]
    if let Some(path) = nonempty_env_path("LOCALAPPDATA") {
        return Ok(path.join("Synapse"));
    }

    #[cfg(target_os = "macos")]
    if let Some(path) = nonempty_env_path("HOME") {
        return Ok(path
            .join("Library")
            .join("Application Support")
            .join("Synapse"));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(path) = nonempty_env_path("XDG_DATA_HOME") {
            return Ok(path.join("synapse"));
        }
        if let Some(path) = nonempty_env_path("HOME") {
            return Ok(path.join(".local").join("share").join("synapse"));
        }
    }

    Err("cannot determine Synapse data directory; set SYNAPSE_STORE".to_owned())
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn usage() -> String {
    concat!(
        "usage: synapse --version | ",
        "synapse doctor | ",
        "synapse authorization init <client-id> | ",
        "synapse ipc serve [--once] | synapse ipc trust <client-id> <executable-path> | synapse ipc ping | ",
        "synapse knowledge create <id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content> [--scope <scope> --key <key>] | ",
        "synapse knowledge add <id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content> [--scope <scope> --key <key>] | ",
        "synapse knowledge replace <new-id> <kind> <source> <observed|inferred> <unknown|low|medium|high> <content> <relation-id> <old-id> [--scope <scope> --key <key>] | ",
        "synapse knowledge show <id> | ",
        "synapse knowledge find [<text>] [--kind <kind>] [--source <source>] [--scope <scope>] [--key <key>] [--state <active|stale|conflicted|superseded|unknown|any>] [--basis <observed|inferred|any>] [--limit <1..10>] | ",
        "synapse knowledge relate <relation-id> <subject-id> <supersedes|conflicts> <object-id> <source> <observed|inferred> | ",
        "synapse knowledge status <id> | ",
        "synapse knowledge index rebuild"
    )
    .to_owned()
}
