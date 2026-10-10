fn main() {
    inject_wifi();
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new()
            .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer),
    )
    .unwrap();
}

fn inject_wifi() {
    println!("cargo:rerun-if-env-changed=WIFI_SSID");
    println!("cargo:rerun-if-env-changed=WIFI_PASSWORD");
    println!("cargo:rerun-if-env-changed=WIFI_SSID2");
    println!("cargo:rerun-if-env-changed=WIFI_PASSWORD2");
    println!("cargo:rerun-if-changed=wifi.env");
    let file = std::fs::read_to_string("wifi.env").unwrap_or_default();
    let (ssid, password) = wifi_cred("WIFI_SSID", "WIFI_PASSWORD", &file);
    let (ssid2, password2) = wifi_cred("WIFI_SSID2", "WIFI_PASSWORD2", &file);
    println!("cargo:rustc-env=WIFI_SSID={ssid}");
    println!("cargo:rustc-env=WIFI_PASSWORD={password}");
    println!("cargo:rustc-env=WIFI_SSID2={ssid2}");
    println!("cargo:rustc-env=WIFI_PASSWORD2={password2}");
}

fn wifi_cred(ssid_key: &str, pass_key: &str, file: &str) -> (String, String) {
    (
        sanitize_wifi(&first_set(ssid_key, file), 32),
        sanitize_wifi(&first_set(pass_key, file), 64),
    )
}

fn first_set(key: &str, file: &str) -> String {
    if let Ok(value) = std::env::var(key) {
        if !value.is_empty() {
            return value;
        }
    }
    for line in file.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == key {
            return v.trim().trim_matches('"').to_string();
        }
    }
    String::new()
}

fn sanitize_wifi(s: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for c in s.chars().filter(|c| !c.is_control()) {
        let mut buf = [0u8; 4];
        let enc = c.encode_utf8(&mut buf);
        if out.len() + enc.len() > max_bytes {
            break;
        }
        out.push(c);
    }
    out
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=-Wl,--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}
