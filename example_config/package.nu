source "arch.nu" # split your config into multiple files and source them
source "flatpak.nu"
source "cargo.nu"


let total_packages = {
  Arch: $arch_packages,
  Flatpak: $flatpak_packages,
  Cargo: $cargo_packages,
}

$total_packages # the return value of package.nu is parsed as a record by supac
