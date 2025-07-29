<!-- README_en.md -->

<p align="right">
  <a href=".github/doc/README_zh.md">🇨🇳 中文</a>
</p>

# 🧙 Declarative Management of OpenWrt Routers with Nix

This project provides a comprehensive and declarative framework for managing the entire lifecycle of OpenWrt routers using [Nix](https://nixos.org/) and [Just](https://github.com/casey/just).
It transforms your router configuration into code, enabling full reproducibility, version control, and automation.

> This is not just a UCI configuration tool — it’s a complete router management solution covering everything from initial setup and firmware upgrades to daily maintenance.

## ✨ Features

- **Declarative Configuration**: Define all UCI settings (network, wireless, firewall, etc.) intuitively using the Nix language. Say goodbye to manual `uci` commands.
- **Complete Device Initialization**: Run `just apply` once to fully configure new devices, set passwords, install SSH keys, and apply all system settings.
- **Automated Firmware Upgrades**: `just upgrade` automatically detects the latest OpenWrt version, downloads the firmware, upgrades the device, and restores your configuration.
- **Secure Secret Management**: Seamlessly integrated with [sops](https://github.com/mozilla/sops) to securely manage and encrypt sensitive information like WiFi passwords and API keys.
- **Package Management**: Declare packages to install via `opkg` within the Nix config for automated deployment (WIP).

## 🚀 Getting Started

### 1️⃣ Install Dependencies

Make sure the following tools are installed:

- **Nix (with Flakes enabled)**:
  Install Nix following the [official guide](https://nixos.org/download.html) and add the following to your `nix.conf`:

  ```bash
  experimental-features = nix-command flakes
  ```

- **Just (task runner)**:

  ```bash
  nix-env -iA nixpkgs.just
  ```

- **age (used for SOPS encryption)**:

  ```bash
  nix-env -iA nixpkgs.age
  ```

- **Target Device**:
  The default firmware download URL in the `Justfile` is hardcoded for the Linksys E8450 (UBI). If you're using another device, be sure to modify the `sysupgrade_url` in `Justfile`.

### 2️⃣ Initialize the Project

1. Clone the repository:

   ```bash
   git clone https://github.com/Mic92/openwrt-nix.git
   cd openwrt-nix
   ```

2. Configure Secrets (sops):
   - Generate an `age` key pair:

     ```bash
     age-keygen -o age.key
     ```

     Save the `age.key` private key and copy the public key (`age1...`) for configuration use.

   - Create a `.sops.yaml` file:

     ```yaml
     creation_rules:
       - path_regex: secrets.yml
         age:
           - YOUR_AGE_PUBLIC_KEY_HERE
     ```

     > Replace `YOUR_AGE_PUBLIC_KEY_HERE` with your actual public key.

   - Create and encrypt the `secrets.yml` file:

     ```bash
     sops secrets.yml
     ```

     Example content:

     ```yaml
     root_password: "your-super-secret-password"
     wifi_password: "your-wifi-password"
     ```

### 3️⃣ Customize Your Configuration

1. Edit the `Justfile`:
   - Set your router's IP address: `host = "192.168.1.1"`
   - If not using the Linksys E8450, modify `sysupgrade_url` to point to your device’s firmware.

2. Write your Nix configuration:
   - Use `example.nix` as a template.

   - Declare UCI settings and reference secrets via placeholders, e.g.:

     ```nix
     key = "@wifi_password@";
     ```

   - Placeholders will be replaced with actual values from `secrets.yml` during deployment.

### 4️⃣ Deploy and Manage

Use the following commands to manage your router:

- **Apply Configuration (Init/Update):**

  ```bash
  just apply
  ```

- **Upgrade Firmware and Restore Config:**

  ```bash
  just upgrade
  ```

## 🤝 Contributing

PRs and issues are welcome! If you have any suggestions, improvements, or problems, feel free to open an issue.

## 📄 License

This project is licensed under the [MIT License](LICENSE).
