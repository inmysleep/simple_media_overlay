fn main() {
    slint_build::compile("ui/app.slint").unwrap();
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        if let Err(err) = res.compile() {
            println!("cargo:warning=failed to embed app icon: {err}");
        }
    }
}
