# 在 Windows 上使用 Atomic 编译器

## 方案 1：WSL2（推荐，最简单）

WSL2 提供原生 Linux 环境，可直接使用 Linux 版 Atomic 编译器。

### 安装步骤

```powershell
# 1. 管理员 PowerShell 安装 WSL2
wsl --install -d Ubuntu-24.04

# 2. 重启后进入 Ubuntu，安装依赖
sudo apt update && sudo apt install -y build-essential libcurl4-openssl-dev

# 3. 下载 Atomic 编译器（将 atomic-x.x.x-linux-x64.tar.gz 复制到 WSL）
#    Windows 文件在 WSL 中位于 /mnt/c/

# 4. 解压安装
tar xzf atomic-0.1.0-linux-x64.tar.gz
cd atomic-0.1.0-linux-x64
sudo ./install.sh

# 5. 使用
atomic run examples/hello.at
```

在 WSL 中用 VS Code 编辑 `.at` 文件，然后在 WSL 终端中编译运行。

---

## 方案 2：Windows 原生编译

需要手动安装 LLVM 和 Rust 后从源码构建。

### 安装依赖

#### 安装 LLVM 18
```powershell
# 方式 A：从 llvm.org 下载安装器
# https://github.com/llvm/llvm-project/releases/tag/llvmorg-18.1.8
# 下载 LLVM-18.1.8-win64.exe，安装时勾选"Add LLVM to PATH"

# 方式 B：使用 winget
winget install LLVM.LLVM --version 18.1.8
```

#### 安装 Rust
```powershell
# 从 https://rustup.rs 下载 rustup-init.exe 并运行
# 或使用 winget
winget install Rustlang.Rustup
```

#### 验证安装
```powershell
rustc --version
llvm-config --version
```

### 构建编译器

```powershell
# 克隆项目（或解压源码压缩包）
git clone <project-url> Atomic
cd Atomic

# 构建
cargo build --release

# 二进制位置：target\release\atomic.exe

# 测试
cargo test
```

### 使用

```powershell
.\target\release\atomic.exe run examples\hello.at
.\target\release\atomic.exe build examples\hello.at --emit exe
.\target\release\atomic.exe build examples\hello.at --target wasm --emit obj
```

---

## 方案 3：Docker（任何平台）

```bash
# 构建镜像
cat > Dockerfile << 'EOF'
FROM ubuntu:24.04
RUN apt update && apt install -y build-essential curl
COPY atomic-0.1.0-linux-x64.tar.gz /tmp/
RUN cd /tmp && tar xzf atomic-0.1.0-linux-x64.tar.gz && \
    cd atomic-0.1.0-linux-x64 && ./install.sh
WORKDIR /workspace
ENTRYPOINT ["atomic"]
EOF

# 使用
docker build -t atomic .
docker run --rm -v $(pwd):/workspace atomic run hello.at
```

---

## 总结

| 方案 | 难度 | 说明 |
|------|------|------|
| WSL2 | 简单 | Linux 原生体验，无需额外配置 |
| Windows 原生 | 中等 | 需要安装 LLVM 18 + Rust 并从源码构建 |
| Docker | 中等 | 跨平台通用，需要 Docker |
