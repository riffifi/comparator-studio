fn main() {
    #[cfg(windows)]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/comparator.ico");
        resource.compile().expect("Failed to compile Windows executable icon");
    }
}
