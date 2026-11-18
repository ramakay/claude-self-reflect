use super::*;

#[test]
fn production_extractor_preserves_same_named_methods_in_distinct_impls() {
    // Regression for the v9.5 T4 corruption: changing the per-parse node
    // key back to `(repo,file,kind,name)` makes one of these real
    // definitions overwrite the other before witness qualification.
    let src = "struct CodeContext;\nimpl CodeContext {\n    fn is_empty(&self) -> bool { true }\n}\nstruct AstDiff;\nimpl AstDiff {\n    fn is_empty(&self) -> bool { false }\n}\n";
    let fragment = extract_graph_fragment(
        src,
        SupportLang::Rust,
        "/repo/src/ast_analysis.rs",
        "repo",
        "proj",
        "conv",
        "session",
    );
    let methods: Vec<_> = fragment
        .nodes
        .iter()
        .filter(|node| node.kind == "function" && node.name == "is_empty")
        .map(|node| (node.span_start, node.span_end))
        .collect();
    assert_eq!(methods, [(2, 2), (6, 6)]);
}

#[test]
fn production_extractor_preserves_same_name_and_same_line_occurrences() {
    let fragment = extract_graph_fragment(
        "fn duplicate() {} fn duplicate() {}",
        SupportLang::Rust,
        "/repo/src/lib.rs",
        "repo",
        "proj",
        "conv",
        "session",
    );
    let count = fragment
        .nodes
        .iter()
        .filter(|node| node.kind == "function" && node.name == "duplicate")
        .count();
    assert_eq!(count, 2, "line spans alone must not collapse occurrences");
}

#[test]
fn container_spans_rust_distinguishes_two_impl_blocks_same_method_name() {
    // Reproduces the adversarial-gate audit's proven false positive:
    // two unrelated `is_empty` methods in different `impl` blocks in
    // the same file. Without container qualification these collide on
    // bare `is_empty`; with it, they must resolve to distinct
    // container-qualified names.
    let src = "struct CodeContext;\nimpl CodeContext {\n    fn is_empty(&self) -> bool { true }\n}\nstruct AstDiff;\nimpl AstDiff {\n    fn is_empty(&self) -> bool { false }\n}\n";
    let containers = container_spans(src, SupportLang::Rust);
    assert_eq!(containers.len(), 2, "containers: {containers:?}");
    let names: Vec<&str> = containers.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"CodeContext"), "names: {names:?}");
    assert!(names.contains(&"AstDiff"), "names: {names:?}");
    for c in &containers {
        assert_eq!(c.separator, "::");
    }
}

#[test]
fn container_spans_rust_trait_impl_uses_type_not_trait() {
    // `impl Trait for Type` must qualify by `Type`, never `Trait` — the
    // generic first-identifier-child scan (`extract_name_from_def`)
    // would wrongly pick `Trait` here since it appears first in the
    // grammar.
    let src = "struct Foo;\ntrait Greet { fn hello(&self); }\nimpl Greet for Foo {\n    fn hello(&self) {}\n}\n";
    let containers = container_spans(src, SupportLang::Rust);
    assert_eq!(containers.len(), 1, "containers: {containers:?}");
    assert_eq!(containers[0].name, "Foo");
}

#[test]
fn container_spans_rust_strips_generics_and_references() {
    let src =
        "struct Foo<T> { _t: T }\nimpl<T> Foo<T> {\n    fn is_empty(&self) -> bool { true }\n}\n";
    let containers = container_spans(src, SupportLang::Rust);
    assert_eq!(containers.len(), 1, "containers: {containers:?}");
    assert_eq!(containers[0].name, "Foo");
}

#[test]
fn container_spans_typescript_class_methods() {
    let src = "class Foo {\n  isEmpty() { return true; }\n}\nclass Bar {\n  isEmpty() { return false; }\n}\n";
    let containers = container_spans(src, SupportLang::TypeScript);
    assert_eq!(containers.len(), 4, "containers: {containers:?}");
    let names: Vec<&str> = containers.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"Foo"));
    assert!(names.contains(&"Bar"));
    assert!(names.contains(&"Foo.isEmpty"));
    assert!(names.contains(&"Bar.isEmpty"));
    for c in &containers {
        assert_eq!(c.separator, ".");
    }
}

#[test]
fn container_spans_python_class_methods() {
    let src = "class Foo:\n    def is_empty(self):\n        return True\n\nclass Bar:\n    def is_empty(self):\n        return False\n";
    let containers = container_spans(src, SupportLang::Python);
    assert_eq!(containers.len(), 4, "containers: {containers:?}");
    let names: Vec<&str> = containers.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"Foo"));
    assert!(names.contains(&"Bar"));
    assert!(names.contains(&"Foo.is_empty"));
    assert!(names.contains(&"Bar.is_empty"));
}

#[test]
fn container_spans_free_function_has_no_container() {
    let src = "fn free_fn() -> bool { true }\n";
    let containers = container_spans(src, SupportLang::Rust);
    assert!(containers.is_empty(), "containers: {containers:?}");
}

#[test]
fn container_spans_go_is_out_of_scope() {
    // Go is deliberately not covered (method receivers are a different
    // shape from impl/class bodies) — must return empty, never panic.
    let src = "package main\nfunc (r *Receiver) Method() {}\n";
    let containers = container_spans(src, SupportLang::Go);
    assert!(containers.is_empty(), "containers: {containers:?}");
}

#[test]
fn extracts_nodes_and_call_edge_from_rust() {
    let src = "fn foo() {\n    bar();\n}\nfn bar() {}\nstruct Thing {}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "conv_1",
        "sess_1",
    );

    let names: Vec<&str> = frag.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"foo"), "nodes: {names:?}");
    assert!(names.contains(&"bar"), "nodes: {names:?}");
    assert!(names.contains(&"Thing"), "nodes: {names:?}");
    // module node present
    assert!(frag.nodes.iter().any(|n| n.kind == "module"));

    // calls edge foo -> name:bar
    let foo_id = node_id("repo", "a.rs", "function", "foo");
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.src_id == foo_id)
        .expect("foo should have a calls edge");
    assert_eq!(call.dst_id, "name:bar");
    assert_eq!(call.resolved, 0, "calls start unresolved");
    assert_eq!(call.src_file, "a.rs");
    assert_eq!(call.conv_id, "conv_1");

    // defines edges from module
    assert!(frag.edges.iter().any(|e| e.kind == "defines"));
}

#[test]
fn callee_kind_rust_method_vs_direct() {
    // `map_err` is a receiver-based (method) call — must be classified 'method'.
    let method_src = "fn foo() {\n    let r: Result<(), ()> = Err(());\n    let _ = r.map_err(handler);\n}\nfn handler(_e: ()) {}\n";
    let frag = extract_graph_fragment(
        method_src,
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    let kind = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:map_err")
        .expect("map_err calls edge present")
        .callee_kind
        .clone();
    assert_eq!(kind, "method", "receiver call must be classified method");

    // `helper()` is a bare identifier call — must be classified 'direct'.
    let direct_src = "fn foo() {\n    helper();\n}\nfn helper() {}\n";
    let frag2 = extract_graph_fragment(
        direct_src,
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    let kind2 = frag2
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:helper")
        .expect("helper calls edge present")
        .callee_kind
        .clone();
    assert_eq!(
        kind2, "direct",
        "bare identifier call must be classified direct"
    );
}

#[test]
fn callee_kind_typescript_member_vs_bare() {
    let member_src = "function run() {\n    obj.fetch();\n}\n";
    let frag = extract_graph_fragment(
        member_src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let kind = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:fetch")
        .expect("fetch calls edge present")
        .callee_kind
        .clone();
    assert_eq!(kind, "method", "obj.fetch() must be classified method");

    let bare_src = "function run() {\n    fetch();\n}\n";
    let frag2 = extract_graph_fragment(
        bare_src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let kind2 = frag2
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:fetch")
        .expect("fetch calls edge present")
        .callee_kind
        .clone();
    assert_eq!(kind2, "direct", "bare fetch() must be classified direct");
}

#[test]
fn callee_kind_python_method_call() {
    let src = "class Foo:\n    def handler(self):\n        self.run()\n";
    let frag = extract_graph_fragment(src, SupportLang::Python, "a.py", "repo", "proj", "c", "s");
    let kind = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:run")
        .expect("run calls edge present")
        .callee_kind
        .clone();
    assert_eq!(kind, "method", "self.run() must be classified method");
}

#[test]
fn noise_callees_are_not_emitted_as_call_edges() {
    // `foo` calls a real `handle()` plus ubiquitous built-ins.
    let src = "fn foo() {\n    let v: Vec<u8> = Vec::new();\n    v.iter().collect::<Vec<_>>();\n    handle();\n}\nfn handle() {}\n";
    let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
    let call_dsts: Vec<&str> = frag
        .edges
        .iter()
        .filter(|e| e.kind == "calls")
        .map(|e| e.dst_id.as_str())
        .collect();
    assert!(
        call_dsts.contains(&"name:handle"),
        "real callee kept: {call_dsts:?}"
    );
    for noise in ["name:new", "name:iter", "name:collect"] {
        assert!(
            !call_dsts.contains(&noise),
            "{noise} must be filtered: {call_dsts:?}"
        );
    }
}

#[test]
fn body_hash_changes_with_body() {
    let a = extract_graph_fragment(
        "fn foo() { let x = 1; }",
        SupportLang::Rust,
        "a.rs",
        "r",
        "p",
        "c",
        "s",
    );
    let b = extract_graph_fragment(
        "fn foo() { let x = 2; }",
        SupportLang::Rust,
        "a.rs",
        "r",
        "p",
        "c",
        "s",
    );
    let ha = &a.nodes.iter().find(|n| n.name == "foo").unwrap().body_hash;
    let hb = &b.nodes.iter().find(|n| n.name == "foo").unwrap().body_hash;
    assert_ne!(ha, hb, "body hash must track body change");
}

#[test]
fn malformed_source_does_not_panic() {
    let frag = extract_graph_fragment("fn ( {{{ ", SupportLang::Rust, "x.rs", "r", "p", "c", "s");
    // module node always present; no panic is the assertion.
    assert!(frag.nodes.iter().any(|n| n.kind == "module"));
}

fn import_dsts(frag: &GraphFragment) -> Vec<&str> {
    frag.edges
        .iter()
        .filter(|e| e.kind == "imports")
        .map(|e| e.dst_id.as_str())
        .collect()
}

#[test]
fn rust_multi_symbol_use_emits_per_symbol_import_edges() {
    let src = "use std::sync::{Arc, OnceLock};\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "conv_1",
        "sess_1",
    );
    let dsts = import_dsts(&frag);
    assert!(
        dsts.contains(&"name:Arc"),
        "expected name:Arc among {dsts:?}"
    );
    assert!(
        dsts.contains(&"name:OnceLock"),
        "expected name:OnceLock among {dsts:?}"
    );
    for d in &dsts {
        assert!(
            !d.contains("ArcOnce"),
            "old blob concatenation must not appear: {d}"
        );
    }
}

#[test]
fn rust_use_alias_emits_alias_binding() {
    let src = "use foo::Bar as Baz;\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "conv_1",
        "sess_1",
    );
    let dsts = import_dsts(&frag);
    assert!(
        dsts.contains(&"name:Baz"),
        "expected name:Baz among {dsts:?}"
    );
    assert!(
        !dsts
            .iter()
            .any(|d| d.contains("BarBaz") || *d == "name:BarBaz"),
        "must not concatenate path+alias: {dsts:?}"
    );
}

#[test]
fn typescript_default_named_namespace_imports() {
    let src = "import fs from 'fs';\nimport { fileURLToPath } from 'url';\nimport * as path from 'path';\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "conv_1",
        "sess_1",
    );
    let dsts = import_dsts(&frag);
    assert!(dsts.contains(&"name:fs"), "expected name:fs among {dsts:?}");
    assert!(
        dsts.contains(&"name:fileURLToPath"),
        "expected name:fileURLToPath among {dsts:?}"
    );
    assert!(
        dsts.contains(&"name:path"),
        "expected name:path among {dsts:?}"
    );
}

#[test]
fn python_from_import_and_aliased_import() {
    // Spec fixture. `join` is extracted correctly but is also in the frozen
    // NOISE_CALLEES set (string ops), so edge emission filters it — same
    // is_noise_callee gate as calls. Assert raw symbols + non-noise edges.
    let src = "from os.path import join, exists\nimport numpy as np\n";
    let grep = SupportLang::Python.ast_grep(src);
    let root = grep.root();
    let mut raw: Vec<(String, String)> = Vec::new();
    for kind in import_kinds(SupportLang::Python) {
        let matcher = KindMatcher::new(kind, SupportLang::Python);
        for n in root.find_all(&matcher) {
            raw.extend(import_symbols(&n, SupportLang::Python));
        }
    }
    assert!(
        raw.iter().any(|(s, m)| s == "join" && m == "os.path"),
        "AST extraction must yield join from os.path: {raw:?}"
    );
    assert!(
        raw.iter().any(|(s, m)| s == "exists" && m == "os.path"),
        "AST extraction must yield exists from os.path: {raw:?}"
    );
    assert!(
        raw.iter().any(|(s, m)| s == "np" && m == "numpy"),
        "AST extraction must yield np from numpy: {raw:?}"
    );
    assert!(
        is_noise_callee("join"),
        "fixture assumes join is noise-filtered at edge emission"
    );

    let frag = extract_graph_fragment(
        src,
        SupportLang::Python,
        "a.py",
        "repo",
        "proj",
        "conv_1",
        "sess_1",
    );
    let dsts = import_dsts(&frag);
    assert!(
        !dsts.contains(&"name:join"),
        "join is NOISE_CALLEES — must not emit import edge: {dsts:?}"
    );
    assert!(
        dsts.contains(&"name:exists"),
        "expected name:exists among {dsts:?}"
    );
    assert!(dsts.contains(&"name:np"), "expected name:np among {dsts:?}");
}

#[test]
fn import_edges_do_not_swallow_keyword_tokens() {
    let fixtures: &[(&str, SupportLang, &str)] = &[
        (
            "use std::sync::{Arc, OnceLock};\n",
            SupportLang::Rust,
            "a.rs",
        ),
        ("use foo::Bar as Baz;\n", SupportLang::Rust, "b.rs"),
        (
            "import fs from 'fs';\nimport { fileURLToPath } from 'url';\nimport * as path from 'path';\n",
            SupportLang::TypeScript,
            "a.ts",
        ),
        (
            "from os.path import join, exists\nimport numpy as np\n",
            SupportLang::Python,
            "a.py",
        ),
    ];
    for (src, lang, file) in fixtures {
        let frag = extract_graph_fragment(src, *lang, file, "repo", "proj", "conv_1", "sess_1");
        for e in frag.edges.iter().filter(|e| e.kind == "imports") {
            let name = e.dst_id.strip_prefix("name:").unwrap_or(&e.dst_id);
            assert!(
                !name.contains("import") && !name.contains("from") && !name.contains("require"),
                "keyword blob regression in {} dst_id={}: {name}",
                file,
                e.dst_id
            );
        }
    }
}

// ─── Module specifier capture (WCR Phase 5, TASK 1) ───

fn import_evidence<'a>(frag: &'a GraphFragment, dst: &str) -> &'a str {
    frag.edges
        .iter()
        .find(|e| e.kind == "imports" && e.dst_id == dst)
        .unwrap_or_else(|| panic!("no imports edge for {dst}"))
        .evidence
        .as_str()
}

#[test]
fn rust_import_evidence_records_module_path_prefix() {
    let frag = extract_graph_fragment(
        "use std::collections::HashMap;\n",
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert_eq!(
        import_evidence(&frag, "name:HashMap"),
        "from:std::collections"
    );
}

#[test]
fn rust_import_evidence_multi_symbol_use_shares_module() {
    let frag = extract_graph_fragment(
        "use std::sync::{Arc, OnceLock};\n",
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert_eq!(import_evidence(&frag, "name:Arc"), "from:std::sync");
    assert_eq!(import_evidence(&frag, "name:OnceLock"), "from:std::sync");
}

#[test]
fn rust_import_evidence_alias_records_prefix_before_original_name() {
    let frag = extract_graph_fragment(
        "use foo::Bar as Baz;\n",
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert_eq!(import_evidence(&frag, "name:Baz"), "from:foo");
}

#[test]
fn rust_import_evidence_bare_crate_has_no_module_prefix() {
    // `use std;` — `std` IS the module, there is no prefix before it, so
    // evidence stays empty rather than recording a meaningless `from:`.
    let frag = extract_graph_fragment(
        "use std;\n",
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert_eq!(import_evidence(&frag, "name:std"), "");
}

#[test]
fn typescript_import_evidence_records_source_string() {
    let frag = extract_graph_fragment(
        "import fs from 'fs';\nimport { fileURLToPath } from 'url';\nimport * as path from 'path';\nimport { helper } from './util';\n",
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert_eq!(import_evidence(&frag, "name:fs"), "from:fs");
    assert_eq!(import_evidence(&frag, "name:fileURLToPath"), "from:url");
    assert_eq!(import_evidence(&frag, "name:path"), "from:path");
    assert_eq!(import_evidence(&frag, "name:helper"), "from:./util");
}

#[test]
fn python_import_evidence_records_full_dotted_module() {
    let frag = extract_graph_fragment(
        "from os.path import exists\nimport numpy as np\n",
        SupportLang::Python,
        "a.py",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert_eq!(import_evidence(&frag, "name:exists"), "from:os.path");
    assert_eq!(import_evidence(&frag, "name:np"), "from:numpy");
}

#[test]
fn go_import_evidence_records_full_import_path() {
    // Alias must be >=2 chars: `is_noise_callee` drops single-char names
    // (closure-param-style noise), so a `j "encoding/json"` alias would
    // never reach edge emission at all.
    let src = "package main\n\nimport (\n\t\"fmt\"\n\tjs \"encoding/json\"\n)\n\nfunc main() {\n\tfmt.Println(js)\n}\n";
    let frag = extract_graph_fragment(src, SupportLang::Go, "a.go", "repo", "proj", "c", "s");
    assert_eq!(import_evidence(&frag, "name:fmt"), "from:fmt");
    assert_eq!(import_evidence(&frag, "name:js"), "from:encoding/json");
}

#[test]
fn import_module_evidence_is_capped_at_120_chars() {
    let long_segment = "a".repeat(200);
    let src = format!("import {{ thing }} from '{long_segment}';\n");
    let frag = extract_graph_fragment(
        &src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let evidence = import_evidence(&frag, "name:thing");
    // "from:" (5 chars) + at most 120 chars of module.
    assert!(
        evidence.len() <= 5 + 120,
        "evidence must be capped: {} chars: {evidence}",
        evidence.len()
    );
    assert!(evidence.starts_with("from:aaaa"));
}

// ─── Call-site qualifier capture (WCR Phase 6, TASK A) ───

fn call_evidence<'a>(frag: &'a GraphFragment, dst: &str) -> &'a str {
    frag.edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == dst)
        .unwrap_or_else(|| panic!("no calls edge for {dst}"))
        .evidence
        .as_str()
}

#[test]
fn rust_scoped_call_captures_qualifier_as_via_evidence() {
    let src = "fn foo() {\n    let _ = std::time::Instant::now();\n    let _ = fs::read_to_string(\"x\");\n}\n";
    let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
    assert_eq!(
        call_evidence(&frag, "name:now"),
        "via:std::time::Instant",
        "qualifier is everything before the final ::segment"
    );
    assert_eq!(call_evidence(&frag, "name:read_to_string"), "via:fs");
}

#[test]
fn rust_bare_call_has_no_qualifier_and_stays_direct() {
    let src = "fn foo() {\n    helper();\n}\nfn helper() {}\n";
    let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
    let edge = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:helper")
        .unwrap();
    assert_eq!(edge.evidence, "");
    assert_eq!(edge.callee_kind, "direct", "callee_kind stays as-is");
}

#[test]
fn rust_method_call_does_not_capture_a_via_qualifier() {
    // `r.map_err(f)` is a field_expression (method) callee, not a
    // scoped_identifier — TASK A only captures qualifiers for path calls.
    let src = "fn foo() {\n    let r: Result<(), ()> = Err(());\n    let _ = r.map_err(handler);\n}\nfn handler(_e: ()) {}\n";
    let frag = extract_graph_fragment(src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
    let edge = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:map_err")
        .unwrap();
    assert_eq!(edge.evidence, "");
    assert_eq!(edge.callee_kind, "method");
}

#[test]
fn python_attribute_call_captures_dotted_receiver_as_via_evidence() {
    let src = "import json\nimport os.path\n\ndef handler():\n    json.dumps({})\n    os.path.abspath(\".\")\n    self.run()\n";
    let frag = extract_graph_fragment(src, SupportLang::Python, "a.py", "repo", "proj", "c", "s");
    assert_eq!(call_evidence(&frag, "name:dumps"), "via:json");
    assert_eq!(call_evidence(&frag, "name:abspath"), "via:os.path");
    assert_eq!(call_evidence(&frag, "name:run"), "via:self");
    // Python attribute calls are still classified 'method' by callee_kind
    // — capturing a qualifier does not change that (TASK A leaves
    // callee_kind as-is; the resolver's qualifier tier is what changes
    // behavior downstream, see resolver::qualifier_tier).
    let run_edge = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:run")
        .unwrap();
    assert_eq!(run_edge.callee_kind, "method");
}

#[test]
fn typescript_trivial_member_call_captures_object_path_as_via_evidence() {
    // `x.y.z()` — not `a.b.c()`: single-char trailing names (`c`) are
    // dropped by is_noise_callee's len<2 rule regardless of TASK A.
    let src = "function run() {\n    obj.fetch();\n    x.y.helper_call();\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert_eq!(call_evidence(&frag, "name:fetch"), "via:obj");
    assert_eq!(call_evidence(&frag, "name:helper_call"), "via:x.y");
}

#[test]
fn typescript_non_trivial_member_call_has_no_via_evidence() {
    // `(a || b).x()` — the object is not a simple dotted identifier
    // chain; TASK A deliberately skips it rather than guessing.
    let src = "function run() {\n    (a || b).nontrivial_method();\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let edge = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:nontrivial_method");
    if let Some(edge) = edge {
        assert_eq!(edge.evidence, "", "non-trivial object must not be captured");
    }
}

// ─── Dynamic import() keyword noise (WCR Phase 7, TASK D) ───

#[test]
fn dynamic_import_call_is_not_emitted_as_a_calls_edge() {
    let src =
        "async function run() {\n    const mod = await import('./lazy');\n    return mod;\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let call_dsts: Vec<&str> = frag
        .edges
        .iter()
        .filter(|e| e.kind == "calls")
        .map(|e| e.dst_id.as_str())
        .collect();
    assert!(
        !call_dsts.contains(&"name:import"),
        "dynamic import() keyword must not be emitted as a calls edge: {call_dsts:?}"
    );
}

// ─── await-glue callee extraction (WCR truth pass, TASK 1) ───

#[test]
fn await_generic_direct_call_extracts_bare_callee_not_glued_with_await() {
    // Real, live-corpus shape: `await metaFetch<any[]>(...)` — TS parses
    // the call_expression's `function` field as an `await_expression`
    // node whose own text is "await metaFetch" (see
    // `unwrap_await_glued_function`'s doc comment). Before the fix this
    // produced callee `awaitmetaFetch`.
    let src = "async function f() {\n    const x = await metaFetch<any[]>(url, opts);\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let call = frag
        .edges
        .iter()
        .find(|e| {
            e.kind == "calls" && e.dst_id.starts_with("name:") && e.dst_id.contains("etaFetch")
        })
        .expect("a calls edge targeting metaFetch must exist");
    assert_eq!(
        call.dst_id, "name:metaFetch",
        "await-glued generic call must extract the bare callee, not `awaitmetaFetch`"
    );
    assert_eq!(call.callee_kind, "direct");
}

#[test]
fn await_generic_method_call_extracts_bare_callee_and_method_kind() {
    // `await obj.m<T>(...)` — the same await-glue grammar shape, but the
    // real callee expression is a member_expression, so this must
    // classify `method`, not `direct`.
    let src = "async function f(obj) {\n    const x = await obj.method<number>(url);\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id.contains("method"))
        .expect("a calls edge targeting method must exist");
    assert_eq!(call.dst_id, "name:method");
    assert_eq!(
        call.callee_kind, "method",
        "await-glued generic member call must still classify as a method call"
    );
}

#[test]
fn plain_await_call_without_generics_is_unaffected_by_the_unwrap() {
    // The overwhelmingly common (non-generic) shape: `await foo(...)`
    // already had its call_expression's `function` field pointing
    // directly at the bare identifier, with `await_expression` as the
    // call_expression's PARENT, not its function field —
    // `unwrap_await_glued_function` must be a true no-op here.
    let src = "async function f() {\n    const x = await metaFetch(url, opts);\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls")
        .expect("a calls edge must exist");
    assert_eq!(call.dst_id, "name:metaFetch");
    assert_eq!(call.callee_kind, "direct");
}

#[test]
fn via_evidence_is_capped_at_120_chars() {
    let long_segment = "a".repeat(200);
    let src = format!("fn foo() {{\n    {long_segment}::helper_call_fn();\n}}\n");
    let frag = extract_graph_fragment(&src, SupportLang::Rust, "a.rs", "repo", "proj", "c", "s");
    let evidence = call_evidence(&frag, "name:helper_call_fn");
    assert!(
        evidence.len() <= 4 + 120,
        "evidence must be capped: {} chars: {evidence}",
        evidence.len()
    );
    assert!(evidence.starts_with("via:aaaa"));
}

// ─── Top-level const/let/var def nodes (WCR Phase 7, TASK C) ───

fn def_names_of_kind<'a>(frag: &'a GraphFragment, kind: &str) -> Vec<&'a str> {
    frag.nodes
        .iter()
        .filter(|n| n.kind == kind)
        .map(|n| n.name.as_str())
        .collect()
}

#[test]
fn typescript_top_level_const_becomes_a_def_node() {
    let src = "export const COLORS = { primary: 'blue' };\nconst AnalyticsEvents = 1;\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let names = def_names_of_kind(&frag, "const");
    assert!(names.contains(&"COLORS"), "names: {names:?}");
    assert!(names.contains(&"AnalyticsEvents"), "names: {names:?}");

    // A defines edge from the module anchors each const def, same as a
    // function/type def.
    let colors_id = node_id("repo", "a.ts", "const", "COLORS");
    assert!(frag
        .edges
        .iter()
        .any(|e| e.kind == "defines" && e.dst_id == colors_id));
}

#[test]
fn typescript_top_level_let_and_var_become_def_nodes() {
    let src = "let styles = {};\nvar legacyThing = 1;\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let names = def_names_of_kind(&frag, "const");
    assert!(names.contains(&"styles"), "names: {names:?}");
    assert!(names.contains(&"legacyThing"), "names: {names:?}");
}

#[test]
fn javascript_and_tsx_top_level_const_becomes_a_def_node() {
    for lang in [SupportLang::JavaScript, SupportLang::Tsx] {
        let src = "export const widgetConfig = 1;\n";
        let frag = extract_graph_fragment(src, lang, "a.tsx", "repo", "proj", "c", "s");
        let names = def_names_of_kind(&frag, "const");
        assert!(names.contains(&"widgetConfig"), "{lang:?} names: {names:?}");
    }
}

#[test]
fn const_declared_inside_a_function_body_is_not_a_def_node() {
    let src = "function run() {\n    const local_thing = 1;\n    return local_thing;\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let names = def_names_of_kind(&frag, "const");
    assert!(
        !names.contains(&"local_thing"),
        "function-local const must not become a def node: {names:?}"
    );
}

#[test]
fn destructuring_const_pattern_is_skipped() {
    let src =
        "export const { a, b } = obj;\nexport const [c, d] = arr;\nexport const real_one = 1;\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    let names = def_names_of_kind(&frag, "const");
    assert!(
        !names.iter().any(|n| ["a", "b", "c", "d"].contains(n)),
        "destructuring patterns must not yield def nodes: {names:?}"
    );
    assert!(names.contains(&"real_one"), "names: {names:?}");
}

#[test]
fn rust_and_python_do_not_emit_const_def_nodes() {
    let rust_frag = extract_graph_fragment(
        "const MAX: usize = 10;\nfn foo() {}\n",
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert!(def_names_of_kind(&rust_frag, "const").is_empty());

    let py_frag = extract_graph_fragment(
        "SOME_CONST = 1\n\ndef foo():\n    pass\n",
        SupportLang::Python,
        "a.py",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert!(def_names_of_kind(&py_frag, "const").is_empty());
}

// ─── collect_local_bindings (WCR truth pass, TASK 2, X4 tier) ───

/// Test-only projection: just the name half of every `(scope, name)`
/// pair — most of the existing local-binding tests predate scope
/// qualification (X4 adversarial review, Finding 1) and only care
/// whether a name was captured at all, not which scope it landed in.
/// The dedicated scope tests below check the `(scope, name)` pairs
/// directly.
fn names_of(bindings: &BTreeSet<(String, String)>) -> BTreeSet<String> {
    bindings.iter().map(|(_, n)| n.clone()).collect()
}

#[test]
fn ts_local_bindings_cover_params_destructuring_and_catch() {
    let src = "function f({ playTrack, count = 1, ...rest }: Opts, [first, second]: number[]) {\n  const { solo, other: renamed } = obj;\n  const [head, ...tail] = arr;\n  try {} catch (caught) {}\n  const cb = (err) => { return err; };\n}\nconst TOP_LEVEL = 1;\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::TypeScript));
    for expected in [
        "playTrack",
        "count",
        "rest",
        "first",
        "second",
        "solo",
        "renamed",
        "head",
        "tail",
        "caught",
        "err",
        "cb",
    ] {
        assert!(names.contains(expected), "missing {expected}: {names:?}");
    }
    assert!(
        !names.contains("TOP_LEVEL"),
        "top-level consts are already def nodes — must not double-count as local: {names:?}"
    );
}

#[test]
fn ts_local_bindings_js_default_and_object_default_params() {
    // Plain JS shape (no `required_parameter` TS wrapper): `assignment_pattern`
    // / `object_assignment_pattern` directly under `formal_parameters`.
    let src = "function f(width = 1, { height = 2 } = {}) {\n  return width + height;\n}\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::JavaScript));
    assert!(names.contains("width"), "{names:?}");
    assert!(names.contains("height"), "{names:?}");
}

#[test]
fn ts_local_bindings_arrow_closure_param_reject() {
    // The task's own motivating example: `new Promise((resolve, reject) => ...)`.
    let src =
        "function f() {\n  return new Promise((resolve, reject) => {\n    reject();\n  });\n}\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::TypeScript));
    assert!(names.contains("resolve"), "{names:?}");
    assert!(names.contains("reject"), "{names:?}");
}

#[test]
fn python_local_bindings_cover_params_and_body_assignments() {
    let src = "TOP_LEVEL = 1\n\ndef f(a, b=1, *args, **kwargs):\n    local_x = 1\n    local_y, local_z = 2, 3\n    return local_x\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::Python));
    for expected in ["args", "kwargs", "local_x", "local_y", "local_z"] {
        assert!(names.contains(expected), "missing {expected}: {names:?}");
    }
    // `a`/`b` are single-char, filtered by the len>=2 floor.
    assert!(!names.contains("a") && !names.contains("b"));
    assert!(
        !names.contains("TOP_LEVEL"),
        "module-level assignments are out of scope for this tier: {names:?}"
    );
}

#[test]
fn python_local_bindings_typed_params() {
    let src = "def f(count: int, label: str = 'x'):\n    return count\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::Python));
    assert!(names.contains("count"), "{names:?}");
    assert!(names.contains("label"), "{names:?}");
    assert!(
        !names.contains("int") && !names.contains("str"),
        "type annotations are not bindings: {names:?}"
    );
}

#[test]
fn rust_local_bindings_cover_let_and_closures() {
    let src = "fn f() {\n    let simple_x = 1;\n    let (tuple_a, tuple_b) = (1, 2);\n    if let Some(cond_y) = opt {}\n    let closure = |param_p, param_q| param_p + param_q;\n    let Point { field_x: renamed_x, field_y } = pt;\n}\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::Rust));
    for expected in [
        "simple_x",
        "tuple_a",
        "tuple_b",
        "cond_y",
        "param_p",
        "param_q",
        "renamed_x",
        "field_y",
    ] {
        assert!(names.contains(expected), "missing {expected}: {names:?}");
    }
    assert!(
        !names.contains("Point") && !names.contains("Some"),
        "the type/variant path is never a binding: {names:?}"
    );
    assert!(
        !names.contains("field_x"),
        "the renamed struct field's SOURCE name is not the local binding: {names:?}"
    );
}

#[test]
fn rust_local_bindings_do_not_cover_fn_parameters() {
    // Deliberately out of scope for this tier (task spec: "Rust let
    // bindings + closure params").
    let src = "fn f(fn_param: i32) {\n    let _ = fn_param;\n}\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::Rust));
    assert!(
        !names.contains("fn_param"),
        "regular fn params are out of scope for this tier: {names:?}"
    );
}

#[test]
fn local_bindings_empty_for_source_with_no_local_scope() {
    let src = "pub fn top_level_only() -> usize { 1 }\n";
    let names = names_of(&collect_local_bindings(src, SupportLang::Rust));
    assert!(names.is_empty(), "{names:?}");
}

#[test]
fn extract_graph_fragment_populates_local_bindings_from_the_same_parse() {
    // GraphFragment.local_bindings must reflect the SAME parse used for
    // nodes/edges (extract_inner calls the shared root-based collector
    // directly) — verified end-to-end via the public entry point.
    let src = "function f() {\n  const cb = (reject) => { reject(); };\n}\n";
    let frag = extract_graph_fragment(
        src,
        SupportLang::TypeScript,
        "a.ts",
        "repo",
        "proj",
        "c",
        "s",
    );
    // Second X4 adversarial review, Finding 4: `reject` is a closure
    // param nested (anonymously, via the non-top-level `const cb = ...`
    // arrow function) inside named function `f` — its scope is now a
    // CHAIN rooted at "f" (`"f>anon<idx>"`), not the bare "f" this
    // assertion checked before chains existed, and not `""`/`"cb"`.
    assert!(
        frag.local_bindings
            .iter()
            .any(|(scope, name)| name == "reject" && scope.starts_with("f>anon")),
        "reject is a closure param nested (anonymously) inside named function f, \
             so its scope must be a chain rooted at \"f\", not \"\" or \"cb\": {:?}",
        frag.local_bindings
    );
}

// ─── scope qualification (X4 adversarial review, Finding 1) ───

#[test]
fn ts_local_bindings_scope_isolates_sibling_functions() {
    // The exact bug the fix addresses: a `handler` param bound in one
    // function must not collapse into the same name-set as an unrelated
    // sibling function's own (different) `handler` param — each must
    // carry its OWN function's name as scope.
    let src = "function foo(handler) {\n  return handler;\n}\nfunction bar(handler) {\n  return handler;\n}\n";
    let bindings = collect_local_bindings(src, SupportLang::TypeScript);
    assert!(
        bindings.contains(&("foo".to_string(), "handler".to_string())),
        "{bindings:?}"
    );
    assert!(
        bindings.contains(&("bar".to_string(), "handler".to_string())),
        "{bindings:?}"
    );
    // Exactly two entries, not one collapsed (scope, name) pair.
    assert_eq!(bindings.len(), 2, "{bindings:?}");
}

#[test]
fn ts_local_bindings_top_level_const_arrow_fn_is_a_named_scope() {
    // `const loadTrack = () => {...}` at module scope has no identifier
    // CHILD of the arrow_function node itself (unlike `function foo(){}`),
    // but the def-node infrastructure already treats it as a real named
    // def (`const_decl_kinds`/`is_program_level_declaration`) — the scope
    // tagger must name it identically for the resolver's exact-match to
    // find anything. Binding name is deliberately 2+ chars (`value`, not
    // `x`) — `insert_binding_name` has a documented `len >= 2` floor
    // (mirrors `is_noise_callee`'s single-char skip) that would silently
    // drop a single-char name and mask what this test is actually
    // checking (scope attribution, not the length floor).
    let src = "const loadTrack = () => {\n  let value = 1;\n  return value;\n};\n";
    let bindings = collect_local_bindings(src, SupportLang::TypeScript);
    assert!(
        bindings.contains(&("loadTrack".to_string(), "value".to_string())),
        "{bindings:?}"
    );
}

#[test]
fn ts_local_bindings_nested_anonymous_const_attributes_to_enclosing_named_function() {
    // `helper` is a `const`-assigned arrow function, but NOT a top-level
    // one (nested inside named function `outer`) — per spec only a
    // TOP-LEVEL const fn counts as a named scope, so `inner` must
    // attribute to the nearest NAMED ancestor, `outer`, not to `helper`.
    // Binding name is deliberately 2+ chars (`inner`, not `y`) — see the
    // sibling test above for why a single-char name would mask this
    // test's actual assertion behind the unrelated `len >= 2` floor in
    // `insert_binding_name`.
    let src = "function outer() {\n  const helper = () => {\n    let inner = 1;\n    return inner;\n  };\n  return helper();\n}\n";
    let bindings = collect_local_bindings(src, SupportLang::TypeScript);
    // Second X4 adversarial review, Finding 4: `inner` sits inside the
    // anonymous `helper` closure, so its scope is now a CHAIN rooted at
    // "outer" (`"outer>anon<idx>"`), not the bare "outer" this assertion
    // checked before chains existed — it still attributes to the NAMED
    // ancestor `outer`, never to `helper` itself (checked below).
    assert!(
        bindings
            .iter()
            .any(|(scope, name)| name == "inner" && scope.starts_with("outer>anon")),
        "{bindings:?}"
    );
    assert!(
        !bindings.contains(&("helper".to_string(), "inner".to_string())),
        "a non-top-level const-assigned closure is not a named scope: {bindings:?}"
    );
}

#[test]
fn ts_local_bindings_module_level_block_scope_gets_empty_scope() {
    // A `let` inside a top-level `if`/`try` block (not inside ANY named
    // def) must scope to `""` — module-level, per spec.
    let src =
        "if (globalFlag) {\n  let moduleLevelThing = 1;\n  console.log(moduleLevelThing);\n}\n";
    let bindings = collect_local_bindings(src, SupportLang::TypeScript);
    assert!(
        bindings.contains(&(String::new(), "moduleLevelThing".to_string())),
        "{bindings:?}"
    );
}

// ─── parse_clean (X4 adversarial review, Finding 3) ───

#[test]
fn parse_clean_true_for_well_formed_source() {
    let frag = extract_graph_fragment(
        "fn foo() {\n    helper();\n}\n",
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert!(frag.parse_clean, "well-formed source must parse clean");
}

#[test]
fn parse_clean_false_for_source_with_syntax_error() {
    // Malformed trailing content: tree-sitter's error recovery still
    // extracts `foo` as a real def node (this is exactly the
    // "one def still extracts despite an error elsewhere" shape Finding
    // 3 is about), but the tree as a whole must NOT report clean.
    let frag = extract_graph_fragment(
        "fn foo() {\n    helper();\n}\nfn bar( {{{ !!! garbage not rust\n",
        SupportLang::Rust,
        "a.rs",
        "repo",
        "proj",
        "c",
        "s",
    );
    assert!(
        frag.nodes
            .iter()
            .any(|n| n.name == "foo" && n.kind == "function"),
        "the well-formed def must still extract despite the trailing error: {:?}",
        frag.nodes
    );
    assert!(
        !frag.parse_clean,
        "a tree containing ERROR/MISSING nodes anywhere must not report clean"
    );
}

// ─── WCR truth-pass X4 remediation: calls-edge src / local-binding scope
// agreement (real-world TSX repro session) ───
//
// Minimal inline fixtures reproducing the confirmed real-world break:
// `anukriti-mvp-expo/src/context/radio-context.tsx` (`const playTrack =
// useCallback(...)`, called from sibling `useCallback` closures) and
// `app/radio-player.tsx` (multiline object-destructure of a hook's
// return value, and `useState` array-destructured setters, both called
// from inside a `useCallback`/`useFocusEffect` closure). Before the fix,
// `extract_inner`'s calls-edge source-symbol walk broke at the FIRST
// function-kind ancestor even when it was an anonymous closure, falling
// back to the MODULE node instead of continuing outward to the named
// component — while `nearest_named_scope` (backing `local_bindings`)
// already did continue outward. The two disagreed on scope for every
// call made from inside a hook closure (`""`/module vs the component's
// name), so the X4 tier's scope-equality check could never match real,
// disk-verified component-local bindings — see `nearest_def_node`'s doc
// comment, now shared by both walks.

#[test]
fn calls_edge_src_matches_local_binding_scope_for_usecallback_const() {
    // `playTrack` is a component-scoped `const` whose initializer is a
    // CALL expression (`useCallback(...)`), not a bare arrow function —
    // and it is called from a SIBLING `useCallback` closure, not from
    // its own body. Both the binding and the calling edge must resolve
    // to the SAME scope (`Component`), not disagree (module vs
    // `Component`).
    let src = "\
function Component() {\n\
    const playTrack = useCallback((track) => {\n\
        doPlay(track);\n\
    }, []);\n\
    const selectStation = useCallback((category) => {\n\
        playTrack(category);\n\
    }, [playTrack]);\n\
    return selectStation;\n\
}\n";
    let bindings = collect_local_bindings(src, SupportLang::Tsx);
    assert!(
        bindings.contains(&("Component".to_string(), "playTrack".to_string())),
        "playTrack must be a witnessed local binding scoped to Component: {bindings:?}"
    );

    let frag = extract_graph_fragment(src, SupportLang::Tsx, "a.tsx", "repo", "proj", "c", "s");
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:playTrack")
        .expect("playTrack calls edge present");
    let src_name = frag
        .nodes
        .iter()
        .find(|n| n.id == call.src_id)
        .map(|n| n.name.as_str())
        .unwrap_or("<unknown>");
    assert_eq!(
        src_name, "Component",
        "a call from inside a sibling useCallback closure must attribute to the \
             enclosing named component, not fall back to module scope: {frag:?}"
    );
}

#[test]
fn calls_edge_src_matches_local_binding_scope_for_multiline_object_destructure() {
    // Multiline object-destructure of a hook's return value
    // (`app/radio-player.tsx`'s `const { ..., playTrack } = useRadio();`
    // shape), called from inside a `useFocusEffect(useCallback(...))`
    // closure.
    let src = "\
function Screen() {\n\
    const {\n\
        currentTrack,\n\
        playTrack,\n\
    } = useRadio();\n\
    useFocusEffect(\n\
        useCallback(() => {\n\
            if (currentTrack) playTrack(currentTrack);\n\
        }, [currentTrack, playTrack]),\n\
    );\n\
}\n";
    let bindings = collect_local_bindings(src, SupportLang::Tsx);
    assert!(
        bindings.contains(&("Screen".to_string(), "playTrack".to_string())),
        "playTrack must be a witnessed local binding scoped to Screen: {bindings:?}"
    );

    let frag = extract_graph_fragment(src, SupportLang::Tsx, "a.tsx", "repo", "proj", "c", "s");
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:playTrack")
        .expect("playTrack calls edge present");
    let src_name = frag
        .nodes
        .iter()
        .find(|n| n.id == call.src_id)
        .map(|n| n.name.as_str())
        .unwrap_or("<unknown>");
    assert_eq!(
        src_name, "Screen",
        "a call from inside useFocusEffect(useCallback(...)) must attribute to the \
             enclosing named component, not fall back to module scope: {frag:?}"
    );
}

#[test]
fn calls_edge_src_matches_local_binding_scope_for_usestate_array_destructure() {
    // `useState` array destructure (`const [x, setX] = useState(...)`) —
    // the setter is called from inside a `useEffect` closure.
    let src = "\
function Screen() {\n\
    const [purchasing, setPurchasing] = useState(false);\n\
    useEffect(() => {\n\
        setPurchasing(true);\n\
    }, []);\n\
}\n";
    let bindings = collect_local_bindings(src, SupportLang::Tsx);
    assert!(
        bindings.contains(&("Screen".to_string(), "setPurchasing".to_string())),
        "setPurchasing must be a witnessed local binding scoped to Screen: {bindings:?}"
    );

    let frag = extract_graph_fragment(src, SupportLang::Tsx, "a.tsx", "repo", "proj", "c", "s");
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:setPurchasing")
        .expect("setPurchasing calls edge present");
    let src_name = frag
        .nodes
        .iter()
        .find(|n| n.id == call.src_id)
        .map(|n| n.name.as_str())
        .unwrap_or("<unknown>");
    assert_eq!(
        src_name, "Screen",
        "a call from inside a useEffect closure must attribute to the enclosing \
             named component, not fall back to module scope: {frag:?}"
    );
}

// ─── scope CHAINS (second X4 adversarial review, Finding 4: sibling
// anonymous-closure conflation) ───

#[test]
fn scope_chain_gives_sibling_anonymous_closures_different_chains() {
    // Extraction-level proof of the mechanism `resolver::classify_only`'s
    // prefix match depends on: a `handler` param bound inside ONE
    // anonymous closure, and an UNRELATED `handler()` call inside a
    // SIBLING anonymous closure — both nested in the SAME named
    // function `Component`, so `nearest_def_node`'s GRAPH attribution
    // (unchanged by this finding) flattens both to `Component`. Their
    // scope CHAINS must nonetheless differ.
    let src = "\
function Component() {\n\
    useCallback((handler) => {\n\
        return handler;\n\
    }, []);\n\
    useCallback(() => {\n\
        handler();\n\
    }, []);\n\
}\n";
    let bindings = collect_local_bindings(src, SupportLang::Tsx);
    let (handler_scope, _) = bindings
        .iter()
        .find(|(_, name)| name == "handler")
        .expect("handler param must be a witnessed local binding");
    assert!(
        handler_scope.starts_with("Component>anon"),
        "a param bound inside an anonymous closure must carry a nested chain, \
             not the bare enclosing-def name: {bindings:?}"
    );

    let frag = extract_graph_fragment(src, SupportLang::Tsx, "a.tsx", "repo", "proj", "c", "s");
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:handler")
        .expect("handler() calls edge present");
    // GRAPH src attribution is UNCHANGED by this finding: the call
    // still rolls up to the SAME enclosing named node as the binding.
    let src_name = frag
        .nodes
        .iter()
        .find(|n| n.id == call.src_id)
        .map(|n| n.name.as_str());
    assert_eq!(
        src_name,
        Some("Component"),
        "GRAPH attribution must stay flattened to the named def: {frag:?}"
    );

    let call_chains = frag
        .call_scope_chains
        .get(&(
            call.src_id.clone(),
            call.dst_id.clone(),
            "calls".to_string(),
        ))
        .expect("a calls edge must have a recorded scope chain");
    assert_eq!(
        call_chains.len(),
        1,
        "single physical call site: exactly one chain recorded: {call_chains:?}"
    );
    let call_chain = call_chains.iter().next().unwrap();
    assert!(
        call_chain.starts_with("Component>anon"),
        "the call site is itself inside an anonymous closure: {call_chain}"
    );
    assert_ne!(
        handler_scope, call_chain,
        "sibling closures must get DIFFERENT chains — same bare enclosing def, \
             different within-parse anon index: binding={handler_scope} call={call_chain}"
    );
}

#[test]
fn scope_chain_flows_outer_binding_into_nested_closure_chain() {
    // Counterpart to the sibling test above: a binding declared
    // directly in the named def's OWN body (no closure nesting of its
    // own — chain == the bare name) IS a chain-prefix of a call made
    // from inside a NESTED closure. This is the extraction-level half
    // of `resolver::chain_prefix_matches`'s "outer flows inward" rule.
    let src = "\
function Component() {\n\
    const playTrack = useCallback((track) => {\n\
        doPlay(track);\n\
    }, []);\n\
    useCallback(() => {\n\
        playTrack();\n\
    }, [playTrack]);\n\
}\n";
    let bindings = collect_local_bindings(src, SupportLang::Tsx);
    assert!(
        bindings.contains(&("Component".to_string(), "playTrack".to_string())),
        "playTrack itself is declared directly in Component's body, not inside a \
             closure — its chain must be the bare name: {bindings:?}"
    );

    let frag = extract_graph_fragment(src, SupportLang::Tsx, "a.tsx", "repo", "proj", "c", "s");
    let call = frag
        .edges
        .iter()
        .find(|e| e.kind == "calls" && e.dst_id == "name:playTrack")
        .expect("playTrack() calls edge present");
    let call_chains = frag
        .call_scope_chains
        .get(&(
            call.src_id.clone(),
            call.dst_id.clone(),
            "calls".to_string(),
        ))
        .expect("a calls edge must have a recorded scope chain");
    assert_eq!(
        call_chains.len(),
        1,
        "single physical call site: exactly one chain recorded: {call_chains:?}"
    );
    let call_chain = call_chains.iter().next().unwrap();
    assert!(
        call_chain.starts_with("Component>anon"),
        "the call is made from inside a sibling useCallback closure: {call_chain}"
    );
    assert!(
        call_chain
            .strip_prefix("Component")
            .is_some_and(|rest| rest.starts_with('>')),
        "the call's chain must be nested strictly inside the outer \"Component\" \
             binding's chain (a real chain-segment boundary, not just a text prefix): {call_chain}"
    );
}

#[test]
fn call_scope_chains_records_every_distinct_site_for_an_aggregated_edge() {
    // MANDATED TEST (Codex round 4 adversarial review, Finding 1): the
    // SAME callee invoked from TWO SIBLING anonymous closures aggregates
    // into ONE `code_edges` row (`add_edge`'s own `(src_id, dst_id,
    // kind)` dedup — both calls share the same enclosing named def,
    // `Component`), but `call_scope_chains` must carry BOTH sites' own
    // chains, not just whichever was walked first.
    let src = "\
function Component() {\n\
    useCallback(() => {\n\
        helper();\n\
    }, []);\n\
    useCallback(() => {\n\
        helper();\n\
    }, []);\n\
}\n";
    let frag = extract_graph_fragment(src, SupportLang::Tsx, "a.tsx", "repo", "proj", "c", "s");
    let calls: Vec<_> = frag
        .edges
        .iter()
        .filter(|e| e.kind == "calls" && e.dst_id == "name:helper")
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "both physical call sites aggregate into ONE edge: {calls:?}"
    );
    let call = calls[0];
    assert_eq!(
        call.weight, 2.0,
        "weight still accumulates per physical occurrence, unaffected by this fix"
    );
    let call_chains = frag
        .call_scope_chains
        .get(&(
            call.src_id.clone(),
            call.dst_id.clone(),
            "calls".to_string(),
        ))
        .expect("a calls edge must have recorded scope chains");
    assert_eq!(
        call_chains.len(),
        2,
        "BOTH sibling closures' own chains must be recorded, not just the first: \
             {call_chains:?}"
    );
    for chain in call_chains {
        assert!(
            chain.starts_with("Component>anon"),
            "each recorded chain must be nested inside Component: {chain}"
        );
    }
}
