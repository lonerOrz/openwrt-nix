<!-- README_zh.md -->

<p align="right">
  <a href=".github/doc/README_en.md">🇬🇧 English</a>
</p>

# 🧙 使用 Nix 声明式管理 OpenWrt 路由器

本项目提供了一个全面的、声明式的框架，用于使用 [Nix](https://nixos.org/) 和 [Just](https://github.com/casey/just) 来管理 OpenWrt 路由器的整个生命周期。
它将您的路由器配置转化为代码，实现了完全的可复现性、版本控制和自动化。

> 这不仅仅是一个 UCI 配置工具，更是一个完整的路由器管理方案，涵盖了从初始设置、固件升级到日常维护的所有环节。

## ✨ 核心功能

- **声明式配置**：使用 Nix 语言直观地定义所有 UCI 设置（网络、无线、防火墙等），告别手动 `uci` 命令
- **完整的设备初始化**：一键运行 `just apply`，完成新设备的配置、密码设置、SSH 密钥安装和系统设置应用
- **自动化固件升级**：`just upgrade` 自动检测 OpenWrt 最新版本，下载固件、执行升级并恢复配置
- **安全密钥管理**：与 [sops](https://github.com/mozilla/sops) 无缝集成，安全加密管理 WiFi 密码、API 密钥等敏感信息
- **软件包管理**：通过 Nix 配置声明 `opkg` 安装的软件包，实现自动化部署(WIP)

## 🚀 快速开始

### 1️⃣ 安装依赖

确保你已安装以下工具：

- **Nix（启用 Flakes）**：
  按照 [官方指南](https://nixos.org/download.html) 安装 Nix，并添加以下配置到 `nix.conf`：

  ```bash
  experimental-features = nix-command flakes
  ```

- **Just**（命令任务执行器）：

  ```bash
  nix-env -iA nixpkgs.just
  ```

- **age**（SOPS 加密工具）：

  ```bash
  nix-env -iA nixpkgs.age
  ```

- **目标设备说明**：默认使用 Linksys E8450 (UBI) 固件下载地址。如果你使用其他设备，请修改 `Justfile` 中的 `sysupgrade_url`。

### 2️⃣ 初始化项目

1. 克隆仓库：

   ```bash
   git clone https://github.com/Mic92/openwrt-nix.git
   cd openwrt-nix
   ```

2. 配置密钥（sops）：
   - 生成 `age` 密钥对：

     ```bash
     age-keygen -o age.key
     ```

     保存 `age.key` 私钥，并记录公钥（`age1...`）以备配置使用。

   - 创建 `.sops.yaml` 文件：

     ```yaml
     creation_rules:
       - path_regex: secrets.yml
         age:
           - YOUR_AGE_PUBLIC_KEY_HERE
     ```

     > 替换 `YOUR_AGE_PUBLIC_KEY_HERE` 为你的公钥。

   - 创建并加密 `secrets.yml`：

     ```bash
     sops secrets.yml
     ```

     内容示例：

     ```yaml
     root_password: "your-super-secret-password"
     wifi_password: "your-wifi-password"
     ```

### 3️⃣ 自定义配置

1. 修改 `Justfile`：
   - 设置路由器的 IP 地址：`host = "192.168.1.1"`
   - 如非 Linksys E8450，请修改 `sysupgrade_url` 为你设备的固件地址。

2. 编写 Nix 配置：
   - 使用 `example.nix` 作为模板。

   - 声明 UCI 设置，并使用占位符引用密钥，例如：

     ```nix
     key = "@wifi_password@";
     ```

   - 在部署时占位符将由 `secrets.yml` 中的真实值替换。

### 4️⃣ 部署与管理

使用以下命令管理你的路由器：

- **应用配置（初始化/更新）**：

  ```bash
  just apply
  ```

- **升级固件并恢复配置**：

  ```bash
  just upgrade
  ```

## 🤝 贡献

欢迎 PR 和 Issue！如果你有任何建议、改进或问题，欢迎提出。

## 📄 许可证

本项目使用 [MIT License](LICENSE)。
