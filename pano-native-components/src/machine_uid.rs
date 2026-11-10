// adapted from the machine-uid crate

#[cfg(target_os = "macos")]
pub mod machine_id {
    use std::error::Error;
    use std::process::Command;

    /// Return machine id (IOPlatformUUID), read from ioreg to avoid an IOKit dep.
    pub fn get_machine_id() -> Result<String, Box<dyn Error>> {
        let output = Command::new("/usr/sbin/ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()?;
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains("IOPlatformUUID") {
                if let Some(eq) = line.find('=') {
                    let val = line[eq + 1..].trim().trim_matches('"').trim().to_string();
                    if !val.is_empty() {
                        return Ok(val);
                    }
                }
            }
        }
        Err("IOPlatformUUID not found in ioreg output".into())
    }
}

#[cfg(target_os = "linux")]
pub mod machine_id {
    use std::error::Error;
    use std::fs::File;
    use std::io::Read;

    // dbusPath is the default path for dbus machine id.
    const DBUS_PATH: &str = "/var/lib/dbus/machine-id";
    // or when not found (e.g. Fedora 20)
    const DBUS_PATH_ETC: &str = "/etc/machine-id";

    fn read_file(file_path: &str) -> Result<String, Box<dyn Error>> {
        let mut fd = File::open(file_path)?;
        let mut content = String::new();
        fd.read_to_string(&mut content)?;
        Ok(content.trim().to_string())
    }

    /// Return machine id
    pub fn get_machine_id() -> Result<String, Box<dyn Error>> {
        match read_file(DBUS_PATH) {
            Ok(machine_id) => Ok(machine_id),
            Err(_) => Ok(read_file(DBUS_PATH_ETC)?),
        }
    }
}

#[cfg(target_os = "windows")]
pub mod machine_id {
    use std::error::Error;
    use windows_registry::LOCAL_MACHINE;

    /// Return machine id
    pub fn get_machine_id() -> Result<String, Box<dyn Error>> {
        let crypto = LOCAL_MACHINE.open("SOFTWARE\\Microsoft\\Cryptography")?;
        let id: String = crypto.get_string("MachineGuid")?;

        Ok(id.trim().to_string())
    }
}

pub use machine_id::get_machine_id as get;
