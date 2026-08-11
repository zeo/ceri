use ceri_engine_lab::{Document, Size, Stylesheet, layout};
use std::env;
use std::fs;
use std::path::PathBuf;

struct Options {
    input: PathBuf,
    output: PathBuf,
    viewport: Size,
}

fn main() -> Result<(), String> {
    let options = parse_args(env::args_os().skip(1))?;
    let source = fs::read_to_string(&options.input)
        .map_err(|error| format!("could not read {}: {error}", options.input.display()))?;
    let document = Document::parse(&source).map_err(|error| error.to_string())?;
    let stylesheet = Stylesheet::parse(&document.style_text())
        .map_err(|error| format!("stylesheet error: {error}"))?;
    let layout = layout(&document, &stylesheet, options.viewport);
    let canvas = layout.paint();
    if let Some(parent) = options
        .output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let write_image = match options
        .output
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some(extension) if extension.eq_ignore_ascii_case("bmp") => {
            canvas.write_bmp(&options.output)
        }
        _ => canvas.write_ppm(&options.output),
    };
    write_image
        .map_err(|error| format!("could not write {}: {error}", options.output.display()))?;
    println!("{}", layout.describe());
    println!(
        "painted {}x{} image with {} non-background pixels at {}",
        canvas.width(),
        canvas.height(),
        canvas.non_background_pixels(),
        options.output.display()
    );
    Ok(())
}

fn parse_args<I>(args: I) -> Result<Options, String>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    let mut input = None;
    let mut output = PathBuf::from("target/ceri-engine-lab.ppm");
    let mut width = 800;
    let mut height = 520;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        let argument = argument
            .into_string()
            .map_err(|_| "arguments must be valid UTF-8".to_string())?;
        match argument.as_str() {
            "--input" => input = Some(PathBuf::from(next_value(&mut args, "--input")?)),
            "--output" => output = PathBuf::from(next_value(&mut args, "--output")?),
            "--width" => width = parse_dimension(next_value(&mut args, "--width")?, "width")?,
            "--height" => height = parse_dimension(next_value(&mut args, "--height")?, "height")?,
            "--help" | "-h" => {
                println!(
                    "usage: ceri-engine-lab --input FILE [--output FILE] [--width PX] [--height PX]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let input = input.ok_or_else(|| "--input is required".to_string())?;
    Ok(Options {
        input,
        output,
        viewport: Size {
            width: width as f32,
            height: height as f32,
        },
    })
}

fn next_value<I>(args: &mut I, flag: &str) -> Result<String, String>
where
    I: Iterator<Item = std::ffi::OsString>,
{
    args.next()
        .ok_or_else(|| format!("{flag} requires a value"))?
        .into_string()
        .map_err(|_| format!("{flag} value must be valid UTF-8"))
}

fn parse_dimension(value: String, name: &str) -> Result<u32, String> {
    let dimension = value
        .parse::<u32>()
        .map_err(|_| format!("{name} must be a positive integer"))?;
    if dimension == 0 {
        return Err(format!("{name} must be a positive integer"));
    }
    Ok(dimension)
}
