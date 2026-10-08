// SPDX-License-Identifier: Apache-2.0

//! Offline standards-parser corpus. Python only supplies pip's bundled PEP parser.

use serde_json::{json, Value};
use std::fs;
use std::process::Command;

fn normalize(data: Value) -> Value {
    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input.json");
    fs::write(&input, serde_json::to_vec(&data).unwrap()).unwrap();
    let executable = std::env::var("REZ_TEST_PIP_PYTHON").unwrap_or_else(|_| "python".into());
    let output = Command::new(executable)
        .args(["-E", "-s", "-c", include_str!("../src/pip/packaging.py")])
        .arg(input)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn markers_extras_versions_and_direct_urls_preserve_selected_semantics() {
    let result = normalize(json!({
                "environment": {
                    "python_version":"3.13", "python_full_version":"3.13.11",
                    "sys_platform":"win32", "platform_system":"Windows", "platform_machine":"AMD64"
                },
                "sources":["root[feature]"],
                "distributions":[
                    {"name":"root", "version":"2!1.0alpha1+Ubuntu-01", "requires_dist":[
                        "enabled>=1,!=2.*; python_version >= '3.10'",
                        "disabled; python_version < '3.10'",
                        "child[child_feature]; extra == 'feature'",
                        "literal; 'platform_machine' == 'platform_machine'",
                        "direct @ file:///cache/direct.whl"
                    ]},
                    {"name":"child", "version":"1.2-3", "requires_dist":[
                        "selected; extra == 'child_feature'",
                        "ignored; extra == 'other'",
                        "system; platform_system == 'Windows'"
                    ]}
                ]
    }));
    assert_eq!(result[0]["version"], "1.0.a1-ubuntu.1");
    assert_eq!(result[1]["version"], "1.2.post3");
    assert_eq!(result[0]["extras"], json!(["feature"]));
    assert_eq!(result[1]["extras"], json!(["child_feature"]));
    let deps = result[0]["dependencies"].as_array().unwrap();
    assert_eq!(deps[0]["enabled"], true);
    assert_eq!(deps[1]["enabled"], false);
    assert_eq!(deps[2]["enabled"], true);
    assert_eq!(deps[3]["marker_names"], json!([]));
    assert_eq!(deps[4]["url"], "file:///cache/direct.whl");
    let deps = result[1]["dependencies"].as_array().unwrap();
    assert_eq!(deps[0]["enabled"], true);
    assert_eq!(deps[1]["enabled"], false);
    assert_eq!(deps[2]["marker_names"], json!(["platform_system"]));
}

#[test]
fn portable_markers_tags_and_python_ranges_preserve_semantics() {
    let make = |sources| {
        json!({
            "variant_policy":"none", "environment":{"python_version":"3.13","sys_platform":"win32"},
            "sources":sources,
            "distributions":[
                {"name":"safe", "version":"1", "wheel_tags":["py2.py3-none-any"],
                 "requires_python":">=3.8,<4", "requires_dist":[
                    "optional; (python_version < '3.10' and sys_platform == 'linux') and extra == 'test'",
                    "reverse; 'test' == extra and sys_platform == 'linux'",
                    "plain>=1"
                 ]},
                {"name":"unsafe_or", "version":"1", "wheel_tags":["py3-none-any"],
                 "requires_dist":["optional; extra == 'test' or sys_platform == 'linux'"]},
                {"name":"wrong_abi", "version":"1", "wheel_tags":["cp313-cp313-win_amd64"],
                 "requires_dist":[]},
                {"name":"wrong_interpreter", "version":"1", "wheel_tags":["cp313-none-any"],
                 "requires_dist":[]},
                {"name":"extra_only", "version":"1", "wheel_tags":["py3-none-any"],
                 "requires_dist":["plain; extra == 'feature'"]}
            ]
        })
    };
    let result = normalize(make(json!([])));
    assert_eq!(result[0]["portable"], true);
    assert_eq!(result[0]["dependencies"][0]["enabled"], false);
    assert_eq!(result[0]["dependencies"][1]["enabled"], false);
    assert_eq!(result[0]["dependencies"][2]["enabled"], true);
    assert_eq!(
        result[0]["python_specifiers"],
        json!([
            {"operator":"<","version":"4","wildcard":false},
            {"operator":">=","version":"3.8","wildcard":false}
        ])
    );
    for index in 1..4 {
        assert_eq!(result[index]["portable"], false);
    }
    let active = normalize(make(json!(["safe[test]", "extra_only[feature]"])));
    assert_eq!(active[0]["portable"], false);
    assert_eq!(active[4]["portable"], true);
    assert_eq!(active[4]["dependencies"][0]["enabled"], true);
}
