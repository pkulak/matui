use std::{fs, path::Path, process::Command};

pub fn get_video_duration(path: &Path) -> anyhow::Result<f32> {
    let mut command = Command::new("ffprobe");

    command.args([
        "-loglevel",
        "error",
        "-of",
        "csv=p=0",
        "-show_entries",
        "format=duration",
    ]);

    command.arg(path);

    let output = command.output()?;
    let output = String::from_utf8(output.stdout)?;

    Ok(output.trim().parse()?)
}

pub fn get_video_thumbnail(path: &Path) -> anyhow::Result<Vec<u8>> {
    let duration = get_video_duration(path)?;
    let tmpfile = tempfile::Builder::new().suffix(".jpg").tempfile()?;

    let mut command = Command::new("ffmpeg");

    command.arg("-y");
    command.args(["-loglevel", "error"]);
    command.arg("-ss");
    command.arg((duration / 2.0).to_string());
    command.arg("-i");
    command.arg(path);
    command.args(["-frames:v", "1", "-update", "true"]);
    command.arg(tmpfile.path());

    if !command.status()?.success() {
        anyhow::bail!("could not create thumbnail");
    }

    Ok(fs::read(tmpfile.path())?)
}
