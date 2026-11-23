//! What is built into a machine: mainboard, processor, memory modules, disks, graphics
//! and network adapters. Read once per connection, because none of it changes while
//! the machine runs.

use crate::model::fmt_bytes;
use std::collections::HashMap;

/// One device, or one list of facts if it has no name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HwItem {
    pub name: String,
    pub props: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HwGroup {
    pub title: String,
    /// A remark shown next to the title, e.g. how many memory slots there are.
    pub note: String,
    pub items: Vec<HwItem>,
}

pub type Hardware = Vec<HwGroup>;

/// Linux: the raw output of the usual tools; [`parse`] makes sense of it. `dmidecode`
/// needs root, so the memory modules only show up where `sudo` works without a password.
/// No single quotes, the script may be wrapped in `sh -c '...'`.
const LINUX_SCRIPT: &str = r#"
export LC_ALL=C; d=/sys/class/dmi/id;
echo @@dmi;
for f in sys_vendor product_name product_version board_vendor board_name bios_vendor bios_version bios_date; do echo "$f=$(cat $d/$f 2>/dev/null)"; done;
echo "devtree=$(tr -d "\000" 2>/dev/null </proc/device-tree/model)";
echo "virt=$(systemd-detect-virt 2>/dev/null)";
echo @@lscpu; lscpu 2>/dev/null;
echo @@dmidecode; sudo -n dmidecode -t memory 2>/dev/null;
echo @@lsblk; lsblk -dn -b -P -o NAME,SIZE,ROTA,TRAN,MODEL,TYPE 2>/dev/null;
echo @@lspci; lspci 2>/dev/null;
echo @@netdev; for i in /sys/class/net/*; do test -e $i/device && echo "$(basename $i)|$(cat $i/speed 2>/dev/null)|$(basename "$(readlink $i/device/driver 2>/dev/null)")"; done;
echo @@end
"#;

const MAC_SCRIPT: &str = r#"
export LC_ALL=C;
echo @@sp;
system_profiler SPHardwareDataType SPMemoryDataType SPNVMeDataType SPSerialATADataType SPDisplaysDataType SPEthernetDataType 2>/dev/null;
echo @@end
"#;

/// The script for remote machines and for a local shell: picks the part for its OS.
pub fn script() -> String {
    let (mac, linux) = (MAC_SCRIPT.trim(), LINUX_SCRIPT.trim());
    format!("if test \"$(uname -s)\" = Darwin; then {mac}; else {linux}; fi")
}

/// Windows: PowerShell asks the system's own inventory (CIM) and prints the result in
/// the plain form [`parse`] reads: `@@hw <group>`, `@@item <name>`, `key=value`.
/// One statement per line: PowerShell reads the script from its input line by line.
#[cfg_attr(not(windows), allow(dead_code))]
pub const WINDOWS_SCRIPT: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$gb = { param($bytes) if ($bytes -ge 1TB) { '{0:N1} TB' -f ($bytes / 1TB) } else { '{0:N0} GB' -f ($bytes / 1GB) } }
'@@hw System'; '@@item'
$cs = Get-CimInstance Win32_ComputerSystem; "Manufacturer=$($cs.Manufacturer)"; "Model=$($cs.Model)"
$bb = Get-CimInstance Win32_BaseBoard; "Mainboard=$($bb.Manufacturer) $($bb.Product)"
$bios = Get-CimInstance Win32_BIOS; "BIOS=$($bios.Manufacturer) $($bios.SMBIOSBIOSVersion) ($($bios.ReleaseDate.ToString('yyyy-MM-dd')))"
'@@hw Processor'
Get-CimInstance Win32_Processor | ForEach-Object { "@@item $($_.Name.Trim())"; "Cores=$($_.NumberOfCores)"; "Threads=$($_.NumberOfLogicalProcessors)"; "Base clock=$($_.MaxClockSpeed) MHz"; "Socket=$($_.SocketDesignation)"; "L2 cache=$([math]::Round($_.L2CacheSize / 1024)) MB"; "L3 cache=$([math]::Round($_.L3CacheSize / 1024)) MB" }
'@@hw Memory'
Get-CimInstance Win32_PhysicalMemory | ForEach-Object { "@@item $($_.DeviceLocator)"; "Size=$(& $gb $_.Capacity)"; "Speed=$($_.ConfiguredClockSpeed) MHz"; "Manufacturer=$($_.Manufacturer)"; "Part number=$("$($_.PartNumber)".Trim())" }
'@@hw Disks'
Get-PhysicalDisk | ForEach-Object { "@@item $($_.FriendlyName)"; "Size=$(& $gb $_.Size)"; "Kind=$($_.MediaType)"; "Connection=$($_.BusType)" }
'@@hw Graphics'
Get-CimInstance Win32_VideoController | ForEach-Object { "@@item $($_.Name)"; "Driver=$($_.DriverVersion)"; if ($_.CurrentHorizontalResolution) { "Resolution=$($_.CurrentHorizontalResolution) x $($_.CurrentVerticalResolution) at $($_.CurrentRefreshRate) Hz" } }
'@@hw Network adapters'
Get-CimInstance Win32_NetworkAdapter -Filter 'PhysicalAdapter=True' | Where-Object { $_.PNPDeviceID -match '^(PCI|USB)' } | ForEach-Object { "@@item $($_.Name)"; "Connection=$($_.NetConnectionID)"; if ($_.Speed) { "Speed=$([math]::Round($_.Speed / 1e6)) Mbit/s" } else { 'Speed=no link' } }
'@@end'
"#;

/// Values firmware vendors leave in place of real information.
fn real(value: &str) -> Option<String> {
    let v = value.split_whitespace().collect::<Vec<_>>().join(" ");
    const FILLER: [&str; 9] = [
        "",
        "none",
        "unknown",
        "not specified",
        "default string",
        "to be filled by o.e.m.",
        "system product name",
        "system version",
        "system manufacturer",
    ];
    (!FILLER.contains(&v.to_lowercase().as_str())).then_some(v)
}

fn sections(text: &str) -> HashMap<&str, Vec<&str>> {
    let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut current = "";
    for line in text.lines() {
        match line.strip_prefix("@@") {
            // Only the script's own markers; "@@hw" and "@@item" belong to the Windows format.
            Some(name) if !name.starts_with("hw") && !name.starts_with("item") => {
                current = name.trim();
                map.entry(current).or_default();
            }
            _ if !current.is_empty() => map.entry(current).or_default().push(line),
            _ => {}
        }
    }
    map
}

/// Turn the output of one of the scripts into the list of groups.
pub fn parse(text: &str) -> Hardware {
    let sec = sections(text);
    let mut groups = if sec.contains_key("lscpu") {
        linux(&sec)
    } else if let Some(lines) = sec.get("sp") {
        mac(lines)
    } else {
        plain(text)
    };
    for group in &mut groups {
        group
            .items
            .retain(|item| !item.name.is_empty() || !item.props.is_empty());
    }
    groups.retain(|group| !group.items.is_empty());
    groups
}

/// `@@hw <group>`, `@@item <name>`, `key=value`.
fn plain(text: &str) -> Hardware {
    let mut groups: Hardware = Vec::new();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("@@hw ") {
            groups.push(HwGroup {
                title: title.trim().to_string(),
                ..Default::default()
            });
        } else if let Some(name) = line.strip_prefix("@@item") {
            if let Some(group) = groups.last_mut() {
                group.items.push(HwItem {
                    name: real(name).unwrap_or_default(),
                    ..Default::default()
                });
            }
        } else if let (Some((key, value)), Some(item)) =
            (line.split_once('='), groups.last_mut().and_then(|g| g.items.last_mut()))
            && let Some(value) = real(value)
        {
            item.props.push((key.trim().to_string(), value));
        }
    }
    groups
}

fn group(title: &str, items: Vec<HwItem>) -> HwGroup {
    HwGroup {
        title: title.to_string(),
        note: String::new(),
        items,
    }
}

fn linux(sec: &HashMap<&str, Vec<&str>>) -> Hardware {
    let get = |name: &str| sec.get(name).map(Vec::as_slice).unwrap_or(&[]);
    let mut out = Vec::new();

    // ---- System
    let dmi: HashMap<&str, &str> = get("dmi").iter().filter_map(|l| l.split_once('=')).collect();
    let value = |key: &str| dmi.get(key).and_then(|v| real(v));
    let both = |a: &str, b: &str| {
        real(&format!(
            "{} {}",
            value(a).unwrap_or_default(),
            value(b).unwrap_or_default()
        ))
    };
    let mut system = HwItem::default();
    let add = |item: &mut HwItem, key: &str, value: Option<String>| {
        if let Some(value) = value {
            item.props.push((key.to_string(), value));
        }
    };
    add(&mut system, "Manufacturer", value("sys_vendor"));
    // A version without a product name says nothing.
    add(
        &mut system,
        "Model",
        value("product_name").and_then(|_| both("product_name", "product_version")),
    );
    add(&mut system, "Device", value("devtree"));
    add(&mut system, "Mainboard", both("board_vendor", "board_name"));
    let bios_date = value("bios_date").map(|d| format!(" ({d})")).unwrap_or_default();
    add(
        &mut system,
        "BIOS",
        both("bios_vendor", "bios_version").map(|b| b + &bios_date),
    );
    add(&mut system, "Virtual machine", value("virt"));
    out.push(group("System", vec![system]));

    // ---- Processor: one item per model, a machine may mix two kinds of cores.
    let lscpu: Vec<(&str, &str)> = get("lscpu")
        .iter()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let global = |key: &str| lscpu.iter().find(|(k, _)| *k == key).and_then(|(_, v)| real(v));
    let mut cpus: Vec<HwItem> = Vec::new();
    for (key, value) in &lscpu {
        if *key == "Model name" {
            cpus.push(HwItem {
                name: value.to_string(),
                ..Default::default()
            });
            continue;
        }
        let Some(cpu) = cpus.last_mut() else { continue };
        match *key {
            "Socket(s)" => cpu.props.push(("Sockets".into(), value.to_string())),
            "Core(s) per socket" => cpu.props.push(("Cores per socket".into(), value.to_string())),
            "Thread(s) per core" => cpu.props.push(("Threads per core".into(), value.to_string())),
            "CPU max MHz" => {
                let mhz = value.parse::<f64>().unwrap_or(0.0);
                cpu.props.push(("Max clock".into(), format!("{mhz:.0} MHz")));
            }
            _ => {}
        }
    }
    let single = cpus.len() == 1;
    for cpu in &mut cpus {
        add(cpu, "Architecture", global("Architecture"));
        // Totals for the machine; next to several models they would look like per-model values.
        if single {
            add(cpu, "Logical CPUs", global("CPU(s)"));
            for cache in ["L1d cache", "L2 cache", "L3 cache"] {
                add(cpu, cache, global(cache));
            }
            add(cpu, "Virtualisation", global("Virtualization"));
        }
    }
    out.push(group("Processor", cpus));

    // ---- Memory modules, from dmidecode: blocks of "Key: Value" under "Memory Device".
    let mut modules: Vec<HwItem> = Vec::new();
    let mut note = String::new();
    let mut in_module = false;
    let (mut slots, mut maximum) = (None, None);
    for line in get("dmidecode") {
        let trimmed = line.trim();
        if !line.starts_with(char::is_whitespace) {
            in_module = trimmed == "Memory Device";
            if in_module {
                modules.push(HwItem::default());
            }
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key {
            "Number Of Devices" => slots = Some(value.to_string()),
            "Maximum Capacity" => maximum = Some(value.to_string()),
            _ => {}
        }
        let Some(module) = modules.last_mut().filter(|_| in_module) else {
            continue;
        };
        match key {
            "Locator" => module.name = value.to_string(),
            // Boards with two channels number the slots of each from zero.
            "Bank Locator" => {
                if let Some(bank) = real(value) {
                    module.name = format!("{} ({bank})", module.name);
                }
            }
            "Size" | "Type" | "Speed" | "Manufacturer" => add(module, key, real(value)),
            "Configured Memory Speed" => add(module, "Runs at", real(value)),
            "Part Number" => add(module, "Part number", real(value)),
            _ => {}
        }
    }
    // An empty slot reports "No Module Installed" as its size.
    modules.retain(|m| {
        m.props
            .iter()
            .any(|(k, v)| k == "Size" && v.chars().next().is_some_and(|c| c.is_ascii_digit()))
    });
    if let (Some(slots), Some(maximum)) = (slots, maximum) {
        note = format!("{} of {slots} slots used, up to {maximum}", modules.len());
    }
    if modules.is_empty() {
        let mut item = HwItem::default();
        add(
            &mut item,
            "Modules",
            Some("need root to read: allow sudo dmidecode without a password to list them".into()),
        );
        modules.push(item);
    }
    out.push(HwGroup {
        title: "Memory".into(),
        note,
        items: modules,
    });

    // ---- Disks: NAME="sda" SIZE="..." ROTA="1" TRAN="sata" MODEL="..." TYPE="disk"
    let mut disks = Vec::new();
    for line in get("lsblk") {
        let field = |key: &str| {
            line.split(&format!("{key}=\""))
                .nth(1)
                .and_then(|v| v.split('"').next())
                .unwrap_or("")
                .trim()
        };
        if field("TYPE") != "disk" {
            continue;
        }
        let mut disk = HwItem {
            name: field("NAME").to_string(),
            ..Default::default()
        };
        add(&mut disk, "Model", real(field("MODEL")));
        add(&mut disk, "Size", field("SIZE").parse::<u64>().ok().map(fmt_bytes));
        add(
            &mut disk,
            "Kind",
            Some(if field("ROTA") == "1" { "HDD (spinning)" } else { "SSD" }.to_string()),
        );
        add(&mut disk, "Connection", real(&field("TRAN").to_uppercase()));
        disks.push(disk);
    }
    out.push(group("Disks", disks));

    // ---- PCI devices by class: "01:00.0 VGA compatible controller: NVIDIA Corporation ... (rev a1)"
    let pci = |classes: &[&str]| -> Vec<HwItem> {
        let mut items: Vec<HwItem> = Vec::new();
        for line in get("lspci") {
            let Some((_, rest)) = line.split_once(' ') else {
                continue;
            };
            let Some((class, name)) = rest.split_once(": ") else {
                continue;
            };
            if !classes.contains(&class) {
                continue;
            }
            let name = name.split(" (rev").next().unwrap_or(name).trim().to_string();
            // Several ports of one card are listed once, with a count.
            match items.iter_mut().find(|i| i.name == name) {
                Some(item) => {
                    let count = item.props.first().and_then(|p| p.1.parse::<u32>().ok()).unwrap_or(1) + 1;
                    item.props = vec![("Ports".into(), count.to_string())];
                }
                None => items.push(HwItem {
                    name,
                    ..Default::default()
                }),
            }
        }
        items
    };
    out.push(group(
        "Graphics",
        pci(&["VGA compatible controller", "3D controller", "Display controller"]),
    ));
    out.push(group(
        "Storage controllers",
        pci(&[
            "SATA controller",
            "RAID bus controller",
            "Non-Volatile memory controller",
            "Serial Attached SCSI controller",
        ]),
    ));
    out.push(group(
        "Network adapters",
        pci(&["Ethernet controller", "Network controller", "Infiniband controller"]),
    ));

    // ---- Network ports: "eth0|1000|igb"
    let mut ports = Vec::new();
    for line in get("netdev") {
        let f: Vec<&str> = line.split('|').collect();
        if f.len() != 3 {
            continue;
        }
        let mut port = HwItem {
            name: f[0].to_string(),
            ..Default::default()
        };
        let speed = match f[1].trim().parse::<i64>() {
            Ok(mbit) if mbit >= 1000 && mbit % 1000 == 0 => format!("{} Gbit/s", mbit / 1000),
            Ok(mbit) if mbit > 0 => format!("{mbit} Mbit/s"),
            _ => "no link".to_string(),
        };
        add(&mut port, "Link", Some(speed));
        add(&mut port, "Driver", real(f[2]));
        ports.push(port);
    }
    out.push(group("Network ports", ports));
    out
}

/// `system_profiler` prints an outline: a title without indentation per section, headings
/// ending in a colon for each device, and `Key: Value` lines below them.
fn mac(lines: &[&str]) -> Hardware {
    const SKIP: [&str; 20] = [
        "Serial Number",
        "Hardware UUID",
        "Provisioning UDID",
        "Activation Lock Status",
        "MAC Address",
        "Vendor ID",
        "Product ID",
        "BSD Name",
        "BSD Device Name",
        "Partition Map Type",
        "Revision",
        "Detachable Drive",
        "Removable Media",
        "TRIM Support",
        "OS Loader Version",
        "Model Number",
        "AVB Support",
        "Driver",
        "Mirror",
        "Online",
    ];
    let mut groups: Hardware = Vec::new();
    // Everything indented deeper than a "Volumes:" heading is partitions, not hardware.
    let mut skip_below: Option<usize> = None;
    for line in lines {
        let indent = line.len() - line.trim_start().len();
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(limit) = skip_below {
            if indent > limit {
                continue;
            }
            skip_below = None;
        }
        if indent == 0 {
            let title = match text.trim_end_matches(':') {
                "Hardware" => "System",
                "NVMExpress" | "SATA" | "SATA/SATA Express" => "Disks",
                "Graphics/Displays" => "Graphics",
                "Ethernet" => "Network adapters",
                other => other,
            };
            // NVMe and SATA disks end up in the same group.
            if !groups.iter().any(|g| g.title == title) {
                groups.push(group(title, Vec::new()));
            } else if let Some(position) = groups.iter().position(|g| g.title == title) {
                let existing = groups.remove(position);
                groups.push(existing);
            }
            groups.last_mut().unwrap().items.push(HwItem::default());
            continue;
        }
        let Some(current) = groups.last_mut() else { continue };
        match text.split_once(": ") {
            Some((key, value)) => {
                if SKIP.iter().any(|s| key.starts_with(s)) {
                    continue;
                }
                if let (Some(value), Some(item)) = (real(value), current.items.last_mut()) {
                    item.props.push((key.to_string(), value));
                }
            }
            None if text == "Volumes:" => skip_below = Some(indent),
            None => {
                let name = text.trim_end_matches(':');
                // "Hardware Overview" is the title again, not a device.
                let name = if name.ends_with("Overview") || name == "Displays" {
                    ""
                } else {
                    name
                };
                current.items.push(HwItem {
                    name: name.to_string(),
                    ..Default::default()
                });
            }
        }
    }
    // A heading directly followed by another heading (a controller) has nothing to show.
    for group in &mut groups {
        group.items.retain(|item| !item.props.is_empty());
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(item: &HwItem) -> Vec<(&str, &str)> {
        item.props.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }

    #[test]
    fn scripts_are_safe_to_wrap() {
        assert!(!LINUX_SCRIPT.contains('\''));
        assert!(!MAC_SCRIPT.contains('\''));
    }

    #[test]
    fn reads_a_linux_server() {
        let text = "@@dmi\nsys_vendor=Hetzner\nproduct_name=\nproduct_version=1.0\nboard_vendor=ASRockRack\nboard_name=B450D4U-V1L\n\
            bios_vendor=American Megatrends\nbios_version=L2.12\nbios_date=08/30/2024\ndevtree=\nvirt=none\n\
            @@lscpu\nArchitecture:        x86_64\nCPU(s):              16\nModel name:          AMD Ryzen 7 3700X 8-Core Processor\n\
            Thread(s) per core:  2\nCore(s) per socket:  8\nSocket(s):           1\nCPU max MHz:         4426.1709\n\
            Virtualization:      AMD-V\nL1d cache:           256 KiB (8 instances)\nL2 cache:            4 MiB (8 instances)\nL3 cache:            32 MiB (2 instances)\n\
            @@dmidecode\n# dmidecode 3.5\n\nHandle 0x0009, DMI type 16, 23 bytes\nPhysical Memory Array\n\tMaximum Capacity: 128 GB\n\tNumber Of Devices: 4\n\n\
            Handle 0x0011, DMI type 17, 92 bytes\nMemory Device\n\tSize: 32 GB\n\tLocator: DIMM 0\n\tType: DDR4\n\tSpeed: 3200 MT/s\n\tManufacturer: Samsung\n\
            \tSerial Number: 00000000\n\tPart Number: M391A4G43AB1-CWE    \n\tConfigured Memory Speed: 2666 MT/s\n\n\
            Handle 0x0012, DMI type 17, 92 bytes\nMemory Device\n\tSize: No Module Installed\n\tLocator: DIMM 1\n\tType: Unknown\n\
            @@lsblk\nNAME=\"loop0\" SIZE=\"4096\" ROTA=\"0\" TRAN=\"\" MODEL=\"\" TYPE=\"loop\"\n\
            NAME=\"sda\" SIZE=\"10000831348736\" ROTA=\"1\" TRAN=\"sata\" MODEL=\"ST10000NM0156\" TYPE=\"disk\"\n\
            NAME=\"nvme0n1\" SIZE=\"1000204886016\" ROTA=\"0\" TRAN=\"nvme\" MODEL=\"SAMSUNG MZVL21T0\" TYPE=\"disk\"\n\
            @@lspci\n00:00.0 Host bridge: Advanced Micro Devices, Inc. [AMD] Root Complex\n\
            01:00.0 Ethernet controller: Intel Corporation I210 Gigabit Network Connection (rev 03)\n\
            01:00.1 Ethernet controller: Intel Corporation I210 Gigabit Network Connection (rev 03)\n\
            02:00.0 VGA compatible controller: ASPEED Technology, Inc. ASPEED Graphics Family (rev 41)\n\
            03:00.0 SATA controller: Advanced Micro Devices, Inc. [AMD] FCH SATA Controller (rev 51)\n\
            @@netdev\nenp35s0|1000|igb\nenp36s0|-1|igb\nwlan0||iwlwifi\n@@end\n";
        let hw = parse(text);
        let titles: Vec<&str> = hw.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "System",
                "Processor",
                "Memory",
                "Disks",
                "Graphics",
                "Storage controllers",
                "Network adapters",
                "Network ports"
            ]
        );
        assert_eq!(
            props(&hw[0].items[0]),
            [
                ("Manufacturer", "Hetzner"),
                ("Mainboard", "ASRockRack B450D4U-V1L"),
                ("BIOS", "American Megatrends L2.12 (08/30/2024)")
            ]
        );
        let cpu = &hw[1].items[0];
        assert_eq!(cpu.name, "AMD Ryzen 7 3700X 8-Core Processor");
        assert!(
            props(cpu).contains(&("Max clock", "4426 MHz"))
                && props(cpu).contains(&("L3 cache", "32 MiB (2 instances)"))
        );
        assert_eq!(hw[2].note, "1 of 4 slots used, up to 128 GB");
        assert_eq!(hw[2].items.len(), 1);
        assert_eq!(
            props(&hw[2].items[0]),
            [
                ("Size", "32 GB"),
                ("Type", "DDR4"),
                ("Speed", "3200 MT/s"),
                ("Manufacturer", "Samsung"),
                ("Part number", "M391A4G43AB1-CWE"),
                ("Runs at", "2666 MT/s")
            ]
        );
        assert_eq!(hw[3].items.len(), 2);
        assert_eq!(
            props(&hw[3].items[0]),
            [
                ("Model", "ST10000NM0156"),
                ("Size", "9.1 TiB"),
                ("Kind", "HDD (spinning)"),
                ("Connection", "SATA")
            ]
        );
        assert_eq!(hw[4].items[0].name, "ASPEED Technology, Inc. ASPEED Graphics Family");
        assert_eq!(props(&hw[6].items[0]), [("Ports", "2")]);
        assert_eq!(props(&hw[7].items[0]), [("Link", "1 Gbit/s"), ("Driver", "igb")]);
        assert_eq!(props(&hw[7].items[2]), [("Link", "no link"), ("Driver", "iwlwifi")]);
    }

    #[test]
    fn says_how_to_see_memory_modules_without_root() {
        let hw = parse(
            "@@dmi\n@@lscpu\nModel name: Cortex-X925\nCore(s) per socket: 10\nModel name: Cortex-A725\nCore(s) per socket: 10\n@@dmidecode\n@@end\n",
        );
        let memory = hw.iter().find(|g| g.title == "Memory").unwrap();
        assert!(memory.items[0].props[0].1.contains("sudo dmidecode"));
        // Two kinds of cores: two processors, without machine-wide totals.
        let cpus = &hw.iter().find(|g| g.title == "Processor").unwrap().items;
        assert_eq!(cpus.len(), 2);
        assert_eq!(props(&cpus[1]), [("Cores per socket", "10")]);
    }

    #[test]
    fn reads_a_mac() {
        let text = "@@sp\nHardware:\n\n    Hardware Overview:\n\n      Model Name: MacBook Pro\n      Chip: Apple M5 Max\n\
            \x20     Serial Number (system): XYZ\n      Hardware UUID: 1234\n\nMemory:\n\n      Memory: 128 GB\n      Type: LPDDR5\n\n\
            NVMExpress:\n\n    Apple SSD Controller:\n\n        APPLE SSD AP4096Z:\n\n          Capacity: 4 TB (4.002.222.325.760 bytes)\n\
            \x20         Serial Number: ABC\n          Volumes:\n            Macintosh HD:\n              Capacity: 4 TB\n\n\
            Graphics/Displays:\n\n    Apple M5 Max:\n\n      Chipset Model: Apple M5 Max\n      Displays:\n        Color LCD:\n          Resolution: 3456 x 2234 Retina\n@@end\n";
        let hw = parse(text);
        let titles: Vec<&str> = hw.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["System", "Memory", "Disks", "Graphics"]);
        assert_eq!(
            props(&hw[0].items[0]),
            [("Model Name", "MacBook Pro"), ("Chip", "Apple M5 Max")]
        );
        assert_eq!(props(&hw[1].items[0]), [("Memory", "128 GB"), ("Type", "LPDDR5")]);
        assert_eq!(hw[2].items.len(), 1);
        assert_eq!(hw[2].items[0].name, "APPLE SSD AP4096Z");
        assert_eq!(props(&hw[2].items[0]), [("Capacity", "4 TB (4.002.222.325.760 bytes)")]);
        let names: Vec<&str> = hw[3].items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Apple M5 Max", "Color LCD"]);
    }

    #[test]
    fn reads_the_windows_form() {
        let text = "@@hw System\n@@item\nManufacturer=Micro-Star\nModel=System Product Name\n@@hw Memory\n@@item DIMMA1\nSize=16 GB\n\
            Part number=CMK32GX4M2E3200C16\n@@item DIMMA2\nSize=16 GB\n@@hw Empty\n@@end\n";
        let hw = parse(text);
        assert_eq!(hw.len(), 2);
        assert_eq!(props(&hw[0].items[0]), [("Manufacturer", "Micro-Star")]);
        assert_eq!((hw[1].items.len(), hw[1].items[1].name.as_str()), (2, "DIMMA2"));
    }

    /// Read the hardware of a real machine: `GRIDWATCH_TEST_HOST=user@host cargo test live_hardware -- --ignored --nocapture`
    /// Without the variable it reads this computer (Windows only).
    #[test]
    #[ignore]
    fn live_hardware() {
        let out = match std::env::var("GRIDWATCH_TEST_HOST") {
            Ok(host) => {
                let command = crate::parse::wrap(&script());
                let out = std::process::Command::new("ssh")
                    .args(["-o", "BatchMode=yes", &host, &command])
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&out.stdout).into_owned()
            }
            Err(_) => crate::collector::run_local(
                "powershell",
                &["-NoProfile", "-NonInteractive", "-Command", "-"],
                WINDOWS_SCRIPT,
            )
            .unwrap(),
        };
        let hw = parse(&out);
        for group in &hw {
            println!("== {} {}", group.title, group.note);
            for item in &group.items {
                println!("   {:<28} {:?}", item.name, item.props);
            }
        }
        assert!(hw.len() >= 3);
    }
}
