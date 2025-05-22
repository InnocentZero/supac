source "arch.nu"
source "flatpak.nu"
source "cargo.nu"


let total_packages = {
  Arch: $arch_packages,
  Flatpak: $flatpak_packages,
  Cargo: $cargo_packages,
}

$total_packages
