# crate-deps

Compute a rust crate's dependency tree.

## Usage

```
use crate_deps::Resolver;

let mut resolver = Resolver::new().unwrap();
let (deps, errs) = resolver.dependencies("serde", None).unwrap();
for dep in deps {
    println!("{} {}", dep.name, dep.version);
}
for err in errs {
    println!(
        "couldn't resolve dependencies with feature '{}': {:#}",
        err.name, err.error
    );
}
```
