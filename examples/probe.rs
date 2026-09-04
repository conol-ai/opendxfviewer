//! Dumps what the `dxf` crate actually parses out of a file. Development aid.
use dxf::entities::*;
use dxf::Drawing;

fn main() {
    for f in std::env::args().skip(1) {
        match Drawing::load_file(&f) {
            Ok(d) => {
                let n = d.entities().count();
                println!("{f}: {n} entities, {} blocks, {} layers", d.blocks().count(), d.layers().count());
                for e in d.entities().take(40) {
                    println!("  [{}] {:?}", e.common.layer, EntityKind(&e.specific));
                }
            }
            Err(e) => println!("{f}: PARSE ERROR {e:?}"),
        }
    }
}

struct EntityKind<'a>(&'a EntityType);
impl std::fmt::Debug for EntityKind<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = format!("{:?}", self.0);
        write!(f, "{}", &s[..s.len().min(160)])
    }
}
