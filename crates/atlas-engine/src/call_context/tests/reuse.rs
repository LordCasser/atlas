use super::*;

#[test]
fn retained_source_byte_limit_evicts_independently_of_file_count() {
    let source = format!(
        "void run() {{}} /*{}*/",
        " ".repeat(MAX_CONTEXT_FILE_BYTES / 2)
    );
    // Establish the tiny declarations first: indexing has its own large-file
    // discovery policy, independent of the context reader's 8 MiB bound.
    let (root, store) = fixture(&[
        ("first.cpp", "void run() {}"),
        ("second.cpp", "void run() {}"),
    ]);
    for path in ["first.cpp", "second.cpp"] {
        std::fs::write(root.path().join(path), &source).unwrap();
    }
    let mut session = CallContextSession::new(store.clone(), root.path().into());
    for path in ["first.cpp", "second.cpp", "first.cpp"] {
        let value = session.inspect(path, 5, 8, false, &|| false).unwrap();
        assert_eq!(value.bytes_read, source.len());
        assert_eq!(session.reusable.files.len(), 1);
        assert_eq!(session.reusable.bytes, source.len());
        assert!(!value.items.is_empty());
    }
}

fn material(result: &CallContextResult) -> String {
    format!("{:?}|{:?}", result.items, result.gaps)
}

#[test]
fn selections_reuse_parsing_without_reusing_results_or_conditions() {
    let source = "void sink(int); void run(int first) { if (first) sink(first); }\nvoid other(int second) { sink(second); }\nvoid proto(int);\nvoid broken() { sink(1);";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let mut session = CallContextSession::new(store.clone(), root.path().into());
    for (index, (text, conditions)) in [
        ("sink(first)", true),
        ("sink(second)", true),
        ("proto", false),
        ("broken", false),
        ("run", false),
        ("sink(first)", false),
    ]
    .into_iter()
    .enumerate()
    {
        let start = source.find(text).unwrap() as u32;
        let end = start + text.len() as u32;
        let baseline = inspect_call_context(
            &store,
            root.path(),
            "main.cpp",
            start,
            end,
            conditions,
            &|| false,
        )
        .unwrap();
        let reused = session
            .inspect("main.cpp", start, end, conditions, &|| false)
            .unwrap();
        assert_eq!(material(&baseline), material(&reused), "{text}");
        assert_eq!(reused.bytes_read, if index == 0 { source.len() } else { 0 });
        assert_eq!(reused.files_read, usize::from(index == 0));
    }
}

#[test]
fn cross_file_declarations_remain_and_input_sessions_are_isolated() {
    let source = "#include \"types.hpp\"\nvoid Owner::run() { member.use(); }";
    let header = "struct Value { void use(); }; struct Owner { Value member; void run(); };";
    let (root, store) = fixture(&[("main.cpp", source), ("types.hpp", header)]);
    let start = source.find("use").unwrap() as u32;
    let mut session = CallContextSession::new(store.clone(), root.path().into());
    let first = session
        .inspect("main.cpp", start, start + 3, false, &|| false)
        .unwrap();
    assert!(first.files_read >= 2, "{first:?}");
    let second = session
        .inspect("main.cpp", start, start + 3, false, &|| false)
        .unwrap();
    assert_eq!(material(&first), material(&second));
    assert_eq!(second.bytes_read, 0);
    assert!(
        second
            .items
            .iter()
            .any(|item| item.role == "receiver_binding")
    );
    let (other_root, other_store) = fixture(&[("main.cpp", "void Owner() {}")]);
    let mut other = CallContextSession::new(other_store.clone(), other_root.path().into());
    let result = other.inspect("main.cpp", 5, 10, false, &|| false).unwrap();
    assert_eq!(result.files_read, 1);
    assert_ne!(material(&result), material(&first));
    // A fresh session against the original context must perform its own reads.
    let mut fresh = CallContextSession::new(store, root.path().into());
    assert_eq!(
        fresh
            .inspect("main.cpp", start, start + 3, false, &|| false)
            .unwrap()
            .bytes_read,
        first.bytes_read
    );
}

#[test]
fn unavailable_changed_and_oversized_sources_do_not_become_sticky_failures() {
    let source = "void run() {}";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let mut session = CallContextSession::new(store.clone(), root.path().into());
    assert_eq!(
        session
            .inspect("main.cpp", 5, 8, false, &|| false)
            .unwrap()
            .files_read,
        1
    );
    let path = root.path().join("main.cpp");
    std::fs::remove_file(&path).unwrap();
    let failed = session.inspect("main.cpp", 5, 8, false, &|| false).unwrap();
    assert!(
        failed
            .gaps
            .iter()
            .any(|g| g.code == "source_context_unavailable")
    );
    std::fs::write(&path, "void run();  ").unwrap();
    let changed = session.inspect("main.cpp", 5, 8, false, &|| false).unwrap();
    let baseline =
        inspect_call_context(&store, root.path(), "main.cpp", 5, 8, false, &|| false).unwrap();
    assert_eq!(material(&changed), material(&baseline));
    assert_eq!(changed.files_read, 1);
    std::fs::write(&path, vec![b' '; MAX_CONTEXT_FILE_BYTES + 1]).unwrap();
    let oversized = session.inspect("main.cpp", 5, 8, false, &|| false).unwrap();
    assert_eq!(oversized.bytes_read, 0);
    assert!(oversized.gaps.iter().any(|g| g.message.contains("8 MiB")));
    std::fs::write(&path, source).unwrap();
    assert_eq!(
        session
            .inspect("main.cpp", 5, 8, false, &|| false)
            .unwrap()
            .bytes_read,
        source.len()
    );
}

#[test]
fn cache_hits_obey_analysis_admission_and_cancellation_releases_material() {
    let source = "void run() {}";
    let (root, store) = fixture(&[("main.cpp", source)]);
    let mut session = CallContextSession::new(store.clone(), root.path().into());
    session.inspect("main.cpp", 5, 8, false, &|| false).unwrap();
    let id = store.find_files_by_path_prefix("main.cpp").unwrap()[0].file_id;
    let mut query = Investigation {
        store: &store,
        root: root.path(),
        canceled: &|| false,
        parsed: BTreeMap::new(),
        result: CallContextResult::default(),
        symbol_static: BTreeMap::new(),
        reusable: Some(&mut session.reusable),
        admitted_bytes: MAX_CONTEXT_TOTAL_BYTES - source.len() + 1,
    };
    assert!(query.source(id).err().unwrap().contains("32 MiB"));
    assert_eq!(query.result.bytes_read, 0);
    drop(query);
    assert_eq!(
        session
            .inspect("main.cpp", 5, 8, false, &|| false)
            .unwrap()
            .bytes_read,
        0
    );
    let retained = Arc::downgrade(session.reusable.files.get(&id).unwrap());
    assert!(session.inspect("main.cpp", 5, 8, false, &|| true).is_err());
    assert!(retained.upgrade().is_none());
    assert_eq!(
        session
            .inspect("main.cpp", 5, 8, false, &|| false)
            .unwrap()
            .bytes_read,
        source.len()
    );
    let retained = Arc::downgrade(session.reusable.files.get(&id).unwrap());
    drop(session);
    assert!(retained.upgrade().is_none());
}

#[test]
fn bounded_retention_evicts_and_recomputes_without_losing_material() {
    let files: Vec<_> = (0..17)
        .map(|n| (format!("file{n}.cpp"), format!("void run{n}() {{}}")))
        .collect();
    let borrowed: Vec<_> = files
        .iter()
        .map(|(path, source)| (path.as_str(), source.as_str()))
        .collect();
    let (root, store) = fixture(&borrowed);
    let mut session = CallContextSession::new(store.clone(), root.path().into());
    for (path, _) in &files {
        session.inspect(path, 5, 8, false, &|| false).unwrap();
        assert!(session.reusable.files.len() <= 16);
        assert!(session.reusable.bytes <= MAX_CONTEXT_FILE_BYTES);
    }
    let evicted = store
        .list_files()
        .unwrap()
        .into_iter()
        .find(|file| !session.reusable.files.contains_key(&file.file_id))
        .unwrap();
    let baseline =
        inspect_call_context(&store, root.path(), &evicted.path, 5, 8, false, &|| false).unwrap();
    let repeated = session
        .inspect(&evicted.path, 5, 8, false, &|| false)
        .unwrap();
    assert_eq!(repeated.files_read, 1);
    assert_eq!(material(&baseline), material(&repeated));
}
