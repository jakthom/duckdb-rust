fn main() {
    cc::Build::new()
        .define("UTF8PROC_EXPORTS", None)
        .file("utf8proc/utf8proc.c")
        .compile("utf8proc");
    println!("cargo:rerun-if-changed=utf8proc");
}
