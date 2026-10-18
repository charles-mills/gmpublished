pub(super) fn options(flags: u32, version: i32) -> String {
    let mut options = String::new();
    for (bit, option) in [
        (2, " snap"),
        (16, " post"),
        (if version >= 49 { 0x80 } else { 0x100 }, " realtime"),
    ] {
        if flags & bit != 0 {
            options.push_str(option);
        }
    }
    options
}

#[cfg(test)]
#[path = "flags_tests.rs"]
mod tests;
