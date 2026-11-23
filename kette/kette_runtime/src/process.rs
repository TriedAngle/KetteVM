use std::ffi::OsString;

/// Host process state exposed through `KetteTools.primitives`.
pub struct Process {
    pub script: Option<String>,
    pub arguments: Vec<String>,
    pub executable: Option<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

impl Process {
    pub fn capture() -> Self {
        let arguments: Vec<String> = std::env::args_os().skip(1).map(lossy).collect();
        let (vm_arguments, program_arguments) = split_arguments(&arguments);
        Self {
            script: script(vm_arguments),
            arguments: program_arguments.to_vec(),
            executable: std::env::current_exe()
                .ok()
                .map(|path| lossy(path.into_os_string())),
            cwd: std::env::current_dir()
                .ok()
                .map(|path| lossy(path.into_os_string())),
            env: std::env::vars_os()
                .map(|(key, value)| (lossy(key), lossy(value)))
                .collect(),
        }
    }
}

/// Splits process arguments at the first `--`: VM arguments first, program
/// arguments after it.
pub fn split_arguments(arguments: &[String]) -> (&[String], &[String]) {
    match arguments.iter().position(|argument| argument == "--") {
        Some(index) => (&arguments[..index], &arguments[index + 1..]),
        None => (arguments, &[]),
    }
}

/// The entry file: the last VM argument that is not an option and not the
/// value of `--repl`.
fn script(vm_arguments: &[String]) -> Option<String> {
    let mut skip = false;
    let mut script = None;
    for argument in vm_arguments {
        if skip {
            skip = false;
            continue;
        }
        if argument == "--repl" {
            skip = true;
            continue;
        }
        if argument.starts_with('-') {
            continue;
        }
        script = Some(argument.clone());
    }
    script
}

fn lossy(value: OsString) -> String {
    value.to_string_lossy().into_owned()
}
