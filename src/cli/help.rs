pub fn help_msg() -> &'static str {
    r#"Usage: rusty-jvm [options] <mainclass> [args...]

Options:
    -D<name>=<value>  Set a system property
    -X<option>        JVM options
    -XX:<option>      Advanced JVM options
    --<option>        Java launcher options
    -<option>         Java standard options
    -h, --help        Show this help message
    -V, --version     Show version information

Memory and garbage collection:
    -Xmx<size>                    Set the maximum managed heap size, in bytes or with a
                                  k, m or g suffix (default: a quarter of physical memory)
    -Xlog:gc                      Log each garbage collection to stderr
    -XX:GCAllocThreshold=<count>  Collect after this many allocations (default: 65536,
                                  adapted to the number of live objects)
    -XX:+GCStressAtSafepoints     Collect at every safepoint, for testing the VM (slow)
"#
}
