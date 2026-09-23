use std::path::Path;

use anyhow::Result;
use assert_cmd::cargo;
use assert_fs::prelude::*;
use predicates::prelude::*;

const SUBCOMMAND: &str = "export";

#[test]
fn help() -> Result<()> {
    let mut cmd = cargo::cargo_bin_cmd!();
    let assert = cmd.arg(SUBCOMMAND).arg("--help").assert();

    assert
        .success()
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains(
            "dovi_tool export [OPTIONS] [input_pos]",
        ));
    Ok(())
}

#[test]
fn exports_json() -> Result<()> {
    let mut cmd = cargo::cargo_bin_cmd!();
    let temp = assert_fs::TempDir::new().unwrap();

    let input_rpu = Path::new("assets/tests/fel_orig.bin");
    let output_json = temp.child("RPU_export.json");

    let assert = cmd
        .arg(SUBCOMMAND)
        .arg(input_rpu)
        .arg("--output")
        .arg(output_json.as_ref())
        .assert();

    assert
        .success()
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains("Exporting serialized RPU list..."));

    output_json.assert(predicate::path::is_file());

    Ok(())
}

#[test]
fn export_all_and_scenes() -> Result<()> {
    let mut cmd = cargo::cargo_bin_cmd!();
    let temp = assert_fs::TempDir::new().unwrap();

    let input_rpu = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/tests/fel_orig.bin");
    let all_json = temp.child("RPU_export.json");
    let scenes_file = temp.child("RPU_scenes_test.txt");

    let assert = cmd
        .current_dir(temp.canonicalize().unwrap())
        .arg(SUBCOMMAND)
        .arg(input_rpu)
        .arg("--data")
        .arg(format!("all,scenes={}", scenes_file.to_str().unwrap()))
        .assert();

    assert.success().stderr(predicate::str::is_empty()).stdout(
        predicate::str::contains("Exporting serialized RPU list...")
            .and(predicate::str::contains("Exporting scenes list...")),
    );

    all_json.assert(predicate::path::is_file());
    scenes_file.assert(predicate::path::is_file());

    Ok(())
}

#[test]
fn export_levels_csv() -> Result<()> {
    let mut cmd = cargo::cargo_bin_cmd!();
    let temp = assert_fs::TempDir::new().unwrap();

    let input_rpu =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/tests/mel_variable_l8_length13.bin");
    let l1_file = temp.child("L1_export.csv");
    let l2_file = temp.child("L2_export.csv");

    let assert = cmd
        .current_dir(temp.canonicalize().unwrap())
        .arg(SUBCOMMAND)
        .arg(input_rpu)
        .arg("--levels")
        .arg("level1,level2")
        .assert();

    assert
        .success()
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains(
            "Exporting extension metadata levels 1, 2...",
        ));

    l1_file.assert(predicate::path::is_file());
    l2_file.assert(predicate::path::is_file());

    assert_eq!(
        std::fs::read_to_string(l1_file)?,
        "frame,min_pq,max_pq,avg_pq\n0,0,3100,2048\n"
    );
    assert_eq!(
        std::fs::read_to_string(l2_file)?,
        concat!(
            "frame,target_max_pq,trim_slope,trim_offset,trim_power,trim_chroma_weight,trim_saturation_gain,ms_weight\n",
            "0,2081,1581,2030,1361,2048,1985,2048\n",
            "0,2851,2044,2052,2029,2048,2041,2048\n",
            "0,3079,2070,2049,2129,2048,2048,2048\n"
        )
    );

    Ok(())
}

#[test]
fn export_levels_json() -> Result<()> {
    let mut cmd = cargo::cargo_bin_cmd!();
    let temp = assert_fs::TempDir::new().unwrap();

    let input_rpu =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/tests/mel_variable_l8_length13.bin");
    let l1_file = temp.child("L1_export.json");
    let l2_file = temp.child("L2_export.json");

    let assert = cmd
        .current_dir(temp.canonicalize().unwrap())
        .arg(SUBCOMMAND)
        .arg(input_rpu)
        .arg("-f")
        .arg("json")
        .arg("--levels")
        .arg("level1,level2")
        .assert();

    assert
        .success()
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains(
            "Exporting extension metadata levels 1, 2...",
        ));

    l1_file.assert(predicate::path::is_file());
    l2_file.assert(predicate::path::is_file());

    assert_eq!(
        std::fs::read_to_string(l1_file)?,
        r#"[{"frame":0,"min_pq":0,"max_pq":3100,"avg_pq":2048}]"#
    );
    assert_eq!(
        std::fs::read_to_string(l2_file)?,
        concat!(
            r#"[{"frame":0,"target_max_pq":2081,"trim_slope":1581,"trim_offset":2030,"trim_power":1361,"trim_chroma_weight":2048,"trim_saturation_gain":1985,"ms_weight":2048},"#,
            r#"{"frame":0,"target_max_pq":2851,"trim_slope":2044,"trim_offset":2052,"trim_power":2029,"trim_chroma_weight":2048,"trim_saturation_gain":2041,"ms_weight":2048},"#,
            r#"{"frame":0,"target_max_pq":3079,"trim_slope":2070,"trim_offset":2049,"trim_power":2129,"trim_chroma_weight":2048,"trim_saturation_gain":2048,"ms_weight":2048}]"#
        )
    );

    Ok(())
}

#[test]
fn export_levels_csv_multiple_frames() -> Result<()> {
    let mut cmd = cargo::cargo_bin_cmd!();
    let temp = assert_fs::TempDir::new().unwrap();

    let input_rpu = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/hevc_tests/regular_rpu.bin");
    let l1_file = temp.child("L1_export.csv");

    let assert = cmd
        .current_dir(temp.canonicalize().unwrap())
        .arg(SUBCOMMAND)
        .arg(input_rpu)
        .arg("--levels")
        .arg("level1")
        .assert();

    assert
        .success()
        .stderr(predicate::str::is_empty())
        .stdout(predicate::str::contains(
            "Exporting extension metadata levels 1...",
        ));

    l1_file.assert(predicate::path::is_file());

    let lines = std::fs::read_to_string(&l1_file)?
        .lines()
        .take(6) // limit because the input is large
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        lines,
        concat!(
            "frame,min_pq,max_pq,avg_pq\n",
            "0,0,2828,1120\n",
            "1,0,2828,1120\n",
            "2,0,2828,1120\n",
            "3,0,2828,1120\n",
            "4,0,2828,1120"
        )
    );

    Ok(())
}
