# crate-deps

Compute a rust crate's dependency tree.

## Usage

```
use crate_deps::Resolver;

let mut resolver = Resolver::new().unwrap();
for dep in resolver.dependencies("serde", None).unwrap() {
    println!("{} {}", dep.name, dep.version);
}
```
