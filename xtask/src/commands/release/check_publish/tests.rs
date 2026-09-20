use serde_json::{Value, json};

use super::validate;

fn package(name: &str, publish: &Value, dependencies: &Value) -> Value {
    json!({
        "name": name,
        "publish": publish,
        "dependencies": dependencies,
    })
}

fn dependency(name: &str, req: &str, kind: &Value) -> Value {
    json!({
        "name": name,
        "req": req,
        "path": format!("/workspace/{name}"),
        "kind": kind,
    })
}

#[test]
fn rejects_unpublished_path_dependency_of_published_package() {
    let metadata = json!({
        "packages": [
            package(
                "openlogi-cli",
                &Value::Null,
                &json!([dependency("openlogi-ipc", "^0.7.7", &Value::Null)]),
            ),
            package("openlogi-ipc", &json!([]), &json!([])),
        ],
    });

    let error = validate(&metadata.to_string()).unwrap_err().to_string();

    assert!(error.contains("`openlogi-cli` depends on unpublished path package `openlogi-ipc`"));
}

#[test]
fn rejects_path_dependency_without_registry_version() {
    let metadata = json!({
        "packages": [
            package(
                "openlogi-ipc",
                &Value::Null,
                &json!([dependency("openlogi-core", "*", &Value::Null)]),
            ),
            package("openlogi-core", &Value::Null, &json!([])),
        ],
    });

    let error = validate(&metadata.to_string()).unwrap_err().to_string();

    assert!(error.contains(
        "`openlogi-ipc` path dependency on `openlogi-core` must declare a registry version"
    ));
}

#[test]
fn accepts_publishable_normal_and_build_closure_and_ignores_dev_dependencies() {
    let metadata = json!({
        "packages": [
            package(
                "openlogi-cli",
                &Value::Null,
                &json!([
                    dependency("openlogi-core", "^0.7.8", &Value::Null),
                    dependency("openlogi-build", "^0.7.8", &json!("build")),
                    dependency("openlogi-test", "*", &json!("dev")),
                ]),
            ),
            package("openlogi-core", &Value::Null, &json!([])),
            package("openlogi-build", &Value::Null, &json!([])),
            package("openlogi-test", &json!([]), &json!([])),
        ],
    });

    assert_eq!(validate(&metadata.to_string()).unwrap(), 3);
}
