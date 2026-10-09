//! One-file plugins: the manifest, the default config and the language
//! files go into the `.wasm` itself as custom sections, which the host reads
//! without running the plugin (docs/decyzje.md D-STD-1, D-STD-2).

/// Builds the manifest and, optionally, the default `config.yml` (with its
/// comments) and the language files into the plugin's `.wasm`. Paths are
/// relative to the crate root; a language file is named by its code
/// (`lang/pl.yml` → `pl`).
///
/// ```ignore
/// pumbo_sdk::embed!(
///     manifest = "pumbo-example.yml",
///     config = "assets/config.yml",
///     lang = ["lang/en.yml", "lang/pl.yml"],
/// );
/// ```
///
/// The host reads the manifest from the file, so the plugin is one file
/// (`plugins/<anything>.wasm`); a `<id>.yml` next to it overrides the
/// built-in manifest. At start the host writes the config to
/// `plugins/<id>/config.yml` and each language file to
/// `plugins/<id>/lang/<code>.yml` when there is none, and names the options
/// and texts an existing file lacks.
#[macro_export]
macro_rules! embed {
    (
        manifest = $manifest:literal
        $(, config = $config:literal)?
        $(, lang = [$($lang:literal),* $(,)?])?
        $(,)?
    ) => {
        $crate::__embed_section!("pumbo-manifest", $manifest);
        $($crate::__embed_section!("pumbo-config", $config);)?
        $($($crate::__embed_section!(concat!("pumbo-lang:", $lang), $lang);)*)?
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __embed_section {
    ($section:expr, $path:literal) => {
        // Only WebAssembly has free-form custom sections; native builds
        // (`cargo test`) do not need them.
        #[cfg(target_arch = "wasm32")]
        const _: () = {
            #[allow(unsafe_code)]
            #[unsafe(link_section = $section)]
            #[used]
            static SECTION: [u8; include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $path))
                .len()] = *include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $path));
        };
    };
}
