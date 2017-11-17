extern crate glob;

fn main() {
    js_tests::generate();
}

mod js_tests {
    use glob::glob;
    use std::env;
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::path::Path;

    pub fn generate() {
        match env::var("PROFILE")
            .expect("should have PROFILE env var")
            .as_ref()
        {
            "debug" => println!("cargo:rustc-env=STARLING_TEST_EXECUTABLE=./target/debug/starling"),
            "release" => {
                println!("cargo:rustc-env=STARLING_TEST_EXECUTABLE=./target/release/starling")
            }
            otherwise => panic!("Unknown $PROFILE: '{}'", otherwise),
        }

        let js_files = glob("./tests/js/**/*.js").expect("should create glob iterator OK");

        let out_dir = env::var_os("OUT_DIR").expect("should have the OUT_DIR variable");
        let generated_tests_path = Path::new(&out_dir).join("js_tests.rs");

        let mut generated_tests =
            File::create(generated_tests_path).expect("should create generated tests file OK");

        println!("cargo:rerun-if-changed=./tests/js/");
        for path in js_files {
            let path = path.expect("should have permissions to read globbed files/dirs");
            println!("cargo:rerun-if-changed={}", path.display());

            let opts = TestOptions::read(&path);
            if opts.not_a_test {
                continue;
            }

            writeln!(
                &mut generated_tests,
                r###"
#[test]
fn {name}() {{
    assert_starling_run_file(
        "{path}",
        {expect_success},
        &{stdout_has:?},
    );
}}
                "###,
                name = path.display()
                    .to_string()
                    .chars()
                    .map(|c| match c {
                        'a'...'z' | 'A'...'Z' | '0'...'9' => c,
                        _ => '_',
                    })
                    .collect::<String>(),
                path = path.display(),
                expect_success = !opts.expect_fail,
                stdout_has = opts.stdout_has
            ).expect("should write to generated js tests file OK");
        }
    }

    #[derive(Default)]
    struct TestOptions {
        //# starling-fail
        expect_fail: bool,

        //# starling-stdout-has: ...
        stdout_has: Vec<String>,

        //# starling-not-a-test
        not_a_test: bool,
    }

    impl TestOptions {
        fn read<P: AsRef<Path>>(path: P) -> Self {
            let mut opts = Self::default();

            let file = File::open(path.as_ref()).expect("should open JS file");
            let file = BufReader::new(file);

            for line in file.lines() {
                let line = line.expect("should read a line from the JS file");
                if line.starts_with("//# starling") {
                    if line == "//# starling-fail" {
                        opts.expect_fail = true;
                    } else if line.starts_with("//# starling-stdout-has:") {
                        let expected: String = line.chars()
                            .skip("//# starling-stdout-has:".len())
                            .collect();
                        opts.stdout_has.push(expected.trim().into());
                    } else if line.starts_with("//# starling-not-a-test") {
                        opts.not_a_test = true;
                        break;
                    } else {
                        panic!("Unknown pragma: '{}' in {}", line, path.as_ref().display());
                    }
                } else {
                    break;
                }
            }

            opts
        }
    }
}
