//! Startup commands launched once after the compositor is ready.
//!
//! Commands are listed in `startup_cmds` in the Lua config and spawned
//! unconditionally at startup, independent of any tag.

/// Parse `startup_cmds = { ... }` from Lua globals.
///
/// Each entry may be a bare string `"cmd"` or an argv table `{ "cmd", "arg" }`.
pub fn lua_parse(t: &mlua::Table) -> Vec<Vec<String>> {
    let Ok(arr) = t.get::<mlua::Table>("startup_cmds") else { return Vec::new() };
    let len = arr.raw_len();
    (1..=len)
        .filter_map(|i| {
            let idx = i64::try_from(i).unwrap_or(i64::MAX);
            if let Ok(Some(s)) = arr.get::<Option<String>>(idx) {
                return Some(vec![s]);
            }
            if let Ok(tbl) = arr.get::<mlua::Table>(idx) {
                let n = tbl.raw_len();
                let cmd: Vec<String> = (1..=n)
                    .filter_map(|j| tbl.get::<String>(i64::try_from(j).unwrap_or(i64::MAX)).ok())
                    .collect();
                if !cmd.is_empty() { return Some(cmd); }
            }
            None
        })
        .collect()
}
