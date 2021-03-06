use std::{
    collections::hash_map::Entry,
    collections::HashMap,
    fmt::Debug,
    iter,
    path::{Path, PathBuf},
};

mod config;
mod js;
mod util;
mod wasm;

use self::config::MAX_FILE_SIZE;
use js::JavaScript;
use util::*;
use wasm::WebAssembly;

const WORKER_FILE_NAME: &str = "worker.mjs";

/// If a struct is Parseable, that means that it is able to parse some
/// some given input into Self, with the potential for failure.
/// ```
/// trait Parseable<ParserInput>: Sized {
///    fn parse(input: &ParserInput) -> Result<Self, failure::Error>;
/// }
///
/// struct SmallNumber(u8);
///
/// impl Parseable<u8> for SmallNumber {
///     fn parse(input: &u8) -> Result<Self, failure::Error> {
///         Ok(Self(*input))
///     }
/// }
///
/// let n = SmallNumber::parse(&8);
///
/// assert!(n.is_ok());
/// ```
pub trait Parseable<ParserInput>: Sized {
    fn parse(input: &ParserInput) -> Result<Self, failure::Error>;
}

/// If a struct is Lintable, that means it's able to check itself according to some
/// criteria. It can be linted -- it's lintable!
/// ```
/// trait Lintable {
///     fn lint(&self) -> Result<(), failure::Error>;
/// }
///
/// struct SmallNumber(u8);
///
/// impl Lintable for SmallNumber {
///     fn lint(&self) -> Result<(), failure::Error> {
///         if self.0 > 4 {
///             Err(failure::err_msg("Number is too big!"))
///         } else {
///             Ok(())
///         }
///     }
/// }
///
/// let three = SmallNumber(3);
/// let seven = SmallNumber(7);
///
/// assert!(three.lint().is_ok());
/// assert!(seven.lint().is_err());
/// ```
pub trait Lintable {
    fn lint(&self) -> Result<(), failure::Error>;
}

/// If a struct is both Parseable and Lintable, then it may also implement
/// Validate, which should serve as a one-stop-shop to go from un-parsed
/// input to linted output. A default implmenetation is provided which
/// simply calls `Self::parse(&input).lint()`
/// ```
/// # trait Parseable<ParserInput>: Sized {
/// #    fn parse(input: &ParserInput) -> Result<Self, failure::Error>;
/// # }
/// #
/// # trait Lintable {
/// #     fn lint(&self) -> Result<(), failure::Error>;
/// # }
/// #
/// pub trait Validate<ParserInput>:
///     Lintable + Parseable<ParserInput>
/// {
///         fn validate(input: ParserInput) -> Result<(), failure::Error> {
///             let t = Self::parse(&input)?;
///             t.lint()
///     }
/// }
///
/// struct SmallNumber(u8);
///
/// impl Parseable<u8> for SmallNumber {
///     fn parse(input: &u8) -> Result<Self, failure::Error> {
///         Ok(Self(*input))
///     }
/// }
///
/// impl Lintable for SmallNumber {
///     fn lint(&self) -> Result<(), failure::Error> {
///         if self.0 > 4 {
///             Err(failure::err_msg("Number is too big!"))
///         } else {
///             Ok(())
///         }
///     }
/// }
///
/// impl Validate<u8> for SmallNumber {};
///
/// assert!(SmallNumber::validate(3).is_ok());
/// assert!(SmallNumber::validate(6).is_err());
/// ```
pub trait Validate<ParserInput>: Lintable + Parseable<ParserInput> {
    fn validate(input: ParserInput) -> Result<(), failure::Error> {
        let t = Self::parse(&input)?;
        t.lint()
    }
}

/// Represents the output of any bundler tool.
///
/// I'm not 100% on the format of this struct because I don't
/// have access to the durable objects beta
/// but it seems as though the format is basically:
/// * one `.mjs` file that serves as the entrypoint to the worker,
/// * any number other arbitrary files that can be imported into the worker.
///
/// The example on GitHub, for example, imports HTML,
/// so I think that's fair to assume.
///
/// The ones we execute server-side are JS and WebAssembly, so those
/// get their own `HashMap`s, and any other files can just be assumed
/// to be static e.g. HTML which means they don't need to be `Validate`d.
#[derive(Debug)]
pub struct BundlerOutput {
    /// A PathBuf pointing to worker.mjs, the entrypoint of the worker
    entry_file: PathBuf,
    /// An in-memory representation of the worker entrypoint
    entry: JavaScript,
    /// Other JS files that are executed in the Worker
    javascript: HashMap<PathBuf, JavaScript>,
    /// WebAssembly that gets executed in the Worker
    webassembly: HashMap<PathBuf, WebAssembly>,
    /// Any other files that are imported in the worker (e.g. HTML)
    other_files: Vec<PathBuf>,
}

/// Construct an in-memory representation of a bundler's output given
/// the output dir.
///
/// Starting by parsing <output_dir>/worker.mjs, work through its
/// imports and add those files to the output as necessary.
///
/// Notably, any file emitted by the bundler which is not touched by either
/// worker.mjs or any of its imports (or any of its imports' imports, etc.)
/// will not be added to the resulting BundlerOutput
impl<P: AsRef<Path> + Debug> Parseable<P> for BundlerOutput {
    fn parse(output_dir: &P) -> Result<Self, failure::Error> {
        let entry_file = output_dir.as_ref().join(WORKER_FILE_NAME);
        let entry = JavaScript::parse(&entry_file)?;

        let mut javascript = HashMap::new();
        let mut webassembly = HashMap::new();
        let mut other_files = vec![];

        // Create a stack of the imports in the worker entrypoint
        let mut imports = entry.find_imports();

        // Work through the stack, adding more imports as necessary
        while let Some(import) = imports.pop() {
            let import_path = output_dir.as_ref().join(&import);

            match import_path.extension() {
                None => {
                    // The import is javascript, just without the extension
                    // specified. Add `.js` to the end of it and push it
                    // back on the stack
                    imports.push(format!("{}.js", import));
                }
                Some(extension) => {
                    if let Some(ext_str) = extension.to_str() {
                        match ext_str {
                            "js" | "mjs" => match javascript.entry(import_path.clone()) {
                                Entry::Occupied(_) => continue,
                                Entry::Vacant(entry) => {
                                    let js_import = JavaScript::parse(&import_path)?;
                                    imports.extend(js_import.find_imports());
                                    entry.insert(js_import);
                                }
                            },
                            "wasm" => match webassembly.entry(import_path.clone()) {
                                Entry::Occupied(_) => continue,
                                Entry::Vacant(entry) => {
                                    let wast =
                                        output_dir.as_ref().join(import.replace("wasm", "wast"));
                                    let wat =
                                        output_dir.as_ref().join(import.replace("wasm", "wat"));

                                    let text_file = if wast.is_file() {
                                        Some(wast)
                                    } else if wat.is_file() {
                                        Some(wat)
                                    } else {
                                        None
                                    };

                                    let wasm =
                                        WebAssembly::parse(&(import_path.clone(), text_file))?;

                                    entry.insert(wasm);
                                }
                            },
                            _ => {
                                // Since all we execute server-side is javascript and webassembly,
                                // we can assume these files aren't actually executed.
                                if !other_files.contains(&import_path) {
                                    other_files.push(import_path);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(Self {
            entry_file,
            entry,
            javascript,
            webassembly,
            other_files,
        })
    }
}

/// Check the sizes of all the files the user wants to upload,
/// and then lint them all. I suspect this would be a good
/// use case for rayon, but I'm reluctant to add more dependencies
/// than are absolutely necessary for this PR
impl Lintable for BundlerOutput {
    fn lint(&self) -> Result<(), failure::Error> {
        // Check file sizes
        iter::once(&self.entry_file)
            .chain(self.javascript.keys())
            .chain(self.webassembly.keys())
            .chain(&self.other_files)
            .try_for_each(|file| check_file_size(file, MAX_FILE_SIZE))?;

        // Lint the various files
        iter::once(&self.entry as &dyn Lintable)
            .chain(self.javascript.values().map(|js| js as &dyn Lintable))
            .chain(self.webassembly.values().map(|wasm| wasm as &dyn Lintable))
            .try_for_each(|file| file.lint())
    }
}

impl<P: AsRef<Path> + Debug> Validate<P> for BundlerOutput {}

fn main() -> Result<(), failure::Error> {
    use std::env::current_dir;
    use std::fs::write;

    let dir: PathBuf = current_dir()?.join("minimal");

    let bundle = BundlerOutput::parse(&dir)?;

    write(
        &current_dir()?.join("output"),
        format!("{:#?}", bundle.javascript),
    )?;

    BundlerOutput::validate(&dir)
}
