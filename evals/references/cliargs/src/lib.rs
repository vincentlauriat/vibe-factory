/// Options accepted by the `pack` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub verbose: bool,
    pub output: Option<String>,
    pub jobs: usize,
    pub inputs: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            verbose: false,
            output: None,
            jobs: 1,
            inputs: Vec::new(),
        }
    }
}

fn parse_jobs(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(jobs) if jobs >= 1 && value.bytes().all(|b| b.is_ascii_digit()) => Ok(jobs),
        _ => Err(format!("invalid job count: {value:?}")),
    }
}

/// Parses command-line arguments, excluding the program name.
pub fn parse_args(args: &[&str]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut rest = args.iter();
    while let Some(&arg) = rest.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value)),
            _ => (arg, None),
        };
        let takes_value = matches!(name, "-o" | "--output" | "-j" | "--jobs");
        let value = if takes_value {
            let value = match inline {
                Some(value) if name.starts_with("--") => Some(value),
                Some(_) => None,
                None => rest.next().copied(),
            };
            match value {
                Some(value) if !(inline.is_some() && value.is_empty()) => Some(value),
                _ => return Err(format!("missing value for {name}")),
            }
        } else {
            None
        };
        match (name, value) {
            ("-v" | "--verbose", None) if inline.is_none() => options.verbose = true,
            ("-o" | "--output", Some(value)) => options.output = Some(value.to_string()),
            ("-j" | "--jobs", Some(value)) => options.jobs = parse_jobs(value)?,
            ("--", None) if inline.is_none() => {
                options.inputs.extend(rest.map(|arg| arg.to_string()));
                break;
            }
            _ if arg.starts_with('-') && arg != "-" => {
                return Err(format!("unknown option {arg}"));
            }
            _ => options.inputs.push(arg.to_string()),
        }
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_and_inputs() {
        let options = parse_args(&["-v", "a.txt", "b.txt"]).unwrap();
        assert!(options.verbose);
        assert_eq!(options.inputs, ["a.txt", "b.txt"]);
    }

    #[test]
    fn values_and_errors() {
        let options = parse_args(&["--output=x", "-j", "3", "--", "-v"]).unwrap();
        assert_eq!(options.output.as_deref(), Some("x"));
        assert_eq!(options.jobs, 3);
        assert_eq!(options.inputs, ["-v"]);
        assert!(parse_args(&["-j", "0"]).is_err());
        assert!(parse_args(&["-o"]).unwrap_err().contains("-o"));
        assert!(parse_args(&["-vo"]).unwrap_err().contains("-vo"));
    }
}
