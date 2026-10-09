<p align="center">
  <img src="assets/orca-logo-full.png" alt="Orca" width="200" />
</p>

<h1 align="center">Orca Desktop（奥卡桌面）</h1>

<p align="center">
  <strong>内置 AI 的开源容器管理桌面应用。</strong><br>
  容器、镜像、Compose 堆栈、Kubernetes、AI 助手与 Agent API——一站式管理。
</p>

<p align="center">
  <strong>简体中文</strong> | <a href="./README.md">English</a>
</p>

> [!IMPORTANT]
> **关于本仓库（Fork 说明）**：本仓库是 [edvin/orca](https://github.com/edvin/orca) 的 fork。原项目是一个开源的容器管理桌面应用，内置 AI 助手。本 fork 在上游基础上为桌面应用增加了**简体中文本地化（i18n）**，并提供中文文档。原项目的全部成果属于原作者 [@edvin](https://github.com/edvin) 及上游贡献者，在此致谢。官方发布版本与 Issue 请见[上游仓库](https://github.com/edvin/orca)。由于 fork 无法使用官方签名密钥，本 fork 不发布安装包——请从上游 [Releases](https://github.com/edvin/orca/releases) 页面下载官方构建版本。

<p align="center">
  <img src="screenshots/01-dashboard.png" alt="Orca Desktop 仪表盘" width="800" />
</p>

<p align="center">
  <img src="https://github.com/edvin/orca/actions/workflows/build.yml/badge.svg" alt="Build" />
  <img src="https://img.shields.io/github/license/edvin/orca" alt="License" />
</p>

<p align="center">
  <a href="https://orca-desktop.com">官网</a> · <a href="https://github.com/edvin/orca/releases/latest">下载</a> · 开源项目，基于 Rust、Tauri 与 SolidJS 构建。
</p>

## 本 Fork 的改动

- **简体中文本地化**：内置 i18n 框架（`gui/src/lib/i18n.ts`）与中文语言包 `gui/src/lib/locales/zh-CN.ts`（1600+ 词条），覆盖全部页面与组件
- **语言切换**：`设置 → 偏好设置` 下拉选择，或在命令面板（Ctrl/Cmd+K）中搜索 "Language"；切换即时生效并持久化（`orca.locale`）
- **回退机制**：语言包缺失的词条自动回退显示英文，不会出现空白或报错
- 其余功能与上游完全一致

## 功能特性

### 容器管理

- **完整生命周期**——创建、启动、停止、重启、强杀、删除、**重命名**容器
- **全部停止**——一键停止所有运行中的容器（带确认）
- **运行容器**——支持端口、卷、环境变量、重启策略、CPU/内存限制
- **镜像标签自动补全**——输入 `nginx`，自动提示 Docker Hub 上的 `:latest`、`:alpine`、`:1.27`
- **在线资源编辑**——直接修改运行中容器的内存限制、CPU 核数与重启策略
- **单容器资源图表**——详情页提供全尺寸 CPU/内存时间序列
- **资源用量告警**——内存超过 90% 或 CPU 持续过高时弹出提示
- **Exec 终端**——在容器内打开交互式 Shell
- **实时日志流**——基于 SSE 的日志实时输出与逐层拉取进度条
- **日志查看器**——正则搜索、命中高亮、大小写开关、下载日志
- **多容器日志视图**——合并查看多个容器的彩色日志
- **健康检查**——实时状态指示、健康历史、检查输出
- **重启计数**——徽章显示容器重启次数
- **容器文件浏览器**——浏览运行中容器的文件系统
- **导出为 tar**——保存容器文件系统或镜像为 tar 文件
- **保存为镜像**——把容器当前状态提交为新镜像
- **AI 诊断**——点击任意容器上的 AI 按钮即可分析日志并排查问题
- **复制为 `docker run`** / **导出为 `docker-compose.yml`**
- 实时事件流（容器状态变化即时反映到界面）

### 镜像管理

- **拉取镜像**——Docker Hub 搜索、**逐层进度条**、流式下载
- **构建镜像**——从 Dockerfile 构建，流式输出，支持构建参数与 Dockerfile 选择
- **漏洞扫描**——基于 Trivy 的一键 CVE 扫描，带严重级别徽章
- **镜像层可视化**——堆叠条形图展示 Dockerfile 指令
- **浏览镜像文件**——无需运行即可查看任意镜像的文件系统
- **镜像打标签**——自定义仓库与版本
- **从 tar 导入**——从 tar 归档加载镜像
- **清理（Prune）**未使用镜像，带确认对话框和空间回收报告

### 构建面板

- **构建历史**——持久化日志、状态追踪、耗时统计
- **从 URL 构建**——粘贴 git 仓库或 Dockerfile URL 远程构建
- **构建缓存洞察**——每次构建的缓存命中率可视化
- **构建统计**——成功率、平均耗时、最常构建的镜像
- **构建对比**——两次构建并排对比（参数、配置、耗时）
- **AI 构建调试**——失败构建上一键「问 AI」，自动附带错误与 Dockerfile 上下文
- **orca.yaml 构建目标**——在仓库中声明构建，手动或定时触发
- **定时构建**——基于 cron 的自动构建
- **构建通知**——后台构建完成时弹出通知
- **私有镜像仓库认证**——Docker Hub、GitHub、GitLab、AWS ECR

### Compose 堆栈

- **自动识别**——基于容器标签，无需额外配置文件
- **Compose 编辑器**——用 YAML 编辑器编写 docker-compose.yml 并从界面部署
- **部署前校验**——通过 `docker compose config` 验证，错误内联显示
- 服务健康点 + 堆栈状态汇总（运行中 / 部分运行 / 已停止）
- **Compose up / down / pull**——调用真正的 `docker compose` CLI
- 展开的堆栈视图中可按服务查看日志、启停、重启
- Monaco YAML 编辑器编辑现有 compose 文件，热加载

### Kubernetes（k3s）

- **一键 k3s 集群**——带 Traefik Ingress 控制器与进度对话框
- **20+ 资源类型**：Pods、Deployments、DaemonSets、StatefulSets、ReplicaSets、Services、Ingresses、Jobs、CronJobs、ConfigMaps、Secrets、PVC、PV、存储类、HPA、网络策略、CRD、Helm releases
- **HPA 自动扩缩**——创建、监控、按 CPU 设置最小/最大副本数
- **Secrets 管理**——完整增删改查，支持类型选择（Opaque、TLS、Docker）与内容显隐
- **CRD 浏览器**——列出 CRD 的 group、kind、scope
- **Helm 管理**——查看 releases、安装 Chart、卸载
- **可视化拓扑**——Service → Deployment → Pods 关系图
- **Pod 终端**——进入运行中的 Pod
- **YAML 部署**——Monaco 编辑器语法高亮
- 使用标准 `~/.kube/config` 中的 `orca` 上下文——绝不触碰你的远程集群
- `kubectl --context orca get pods` 开箱即用

### 应用模板

- **一键部署应用**——数据库、Web 服务器、监控、AI、开发工具等
- **社区目录**——模板来自 [orca.9988770.xyz/templates.json](https://orca.9988770.xyz/templates.json)，每小时更新
- 预置合理默认值（端口、卷、环境变量）
- 部署前的端口、环境变量、卷结构化编辑器
- **Compose 堆栈模板**——带有 `compose_yaml` 的多服务模板（如 WordPress + MySQL、Webmail + Stalwart）
- **自动生成密钥**——`generated_env` 在部署时生成随机密码、hex 密钥并自动探测局域网 IP
- **内置证书机构（CA）**——持久化本地 CA 为部署的应用签发 TLS 证书；下载并安装一次 CA 证书即可信任所有 Orca 部署的服务
- **部署后配置向导**——分步引导，支持交互动作（打开 URL、查看日志、执行命令、设置环境变量、重启服务）
- **创建你自己的模板**——保存在本地，与内置模板并存
- **贡献模板**——通过 [PR](CONTRIBUTING.md#contributing-app-templates) 把心爱的应用加入目录
- 密码/密钥类环境变量在编辑器中自动打码

### 网关（反向代理）

- **托管 Caddy 容器**——零配置的自动反向代理
- **`.localhost` 域名**——`webmail.localhost`、`grafana.localhost` 等在所有浏览器可用（RFC 6761）
- **自定义域名**——配置任意基础域名（如配合泛解析的 `*.local.mycompany.dev`）
- **自动 TLS**——由 Orca CA 签发证书，也可自带泛域名证书
- **WebSocket、SSE、HTTP/2**——Caddy 透明代理所有协议
- **基于路径的路由**——同一主机名下把 `/api/*` 和 `/ws/*` 路由到不同容器
- **每个容器的「暴露」按钮**——从任意容器详情页一键注册主机名
- **环境链接**——在 `orca.yaml` 中按分区与环境（本地/预发/生产）分组 URL
- **`orca.yaml`**——项目在仓库中声明网关路由、路径覆盖与环境链接

### AI 助手

- **独立悬浮窗**——可拖拽到任意位置、调整大小、固定到其他显示器
- **5 家提供商**——Claude（Anthropic）、GPT（OpenAI）、Gemini（Google）、Ollama（本地）或任意 OpenAI 兼容端点
- **Ollama 一键部署**——本地 AI，支持 GPU 加速，无需 API 密钥
- **工具调用**——AI 可列出、检查并管理容器资源
- **上下文感知**——点击容器上的 AI 按钮即可带日志开聊
- **模型选择器**——从提供商 API 拉取模型列表
- **会话历史**——滑动窗口式上下文

### AI Agent API

- 面向 Claude Code 与 Claude Desktop 的 **MCP 服务器**
- **OpenAI 兼容的函数调用**端点
- 8 大类 **43 个工具**（容器、镜像、Compose、K8s、卷、网络、系统、诊断）
- 面向自定义 Agent 的直接工具执行端点
- 复合诊断工具（一次调用获取 inspect + 日志 + stats）

### 命令行工具（`orca`）

为脚本化、自动化与团队协作打磨的全功能 CLI。CLI 通过 `127.0.0.1:9477` 访问 Orca 守护进程 API，使用 `ORCA_TOKEN` 环境变量或配置文件中的 token 认证。

#### 容器

```bash
orca containers list                          # 列出所有容器
orca containers start <id>                    # 启动容器
orca containers stop <id>                     # 停止容器
orca containers logs <id> --tail 100          # 查看最后 100 行日志
orca containers exec <id> -- sh -c "ls -la"   # 在容器内运行命令
```

#### 镜像

```bash
orca images list              # 列出所有镜像
orca images pull nginx:alpine # 拉取镜像
orca images remove <id>       # 删除镜像
orca images prune             # 清理未使用的镜像
```

#### 堆栈

```bash
orca stacks list              # 列出 Compose 堆栈
orca stacks up my-stack       # 启动堆栈
orca stacks down my-stack     # 停止堆栈
```

#### 网关

```bash
orca gateway status           # 查看运行状态、域名、端口、路由数量
orca gateway start            # 启动 Caddy 网关容器
orca gateway stop             # 停止网关
orca gateway routes           # 列出所有 主机名 → 容器 映射

orca gateway add webmail webmail-container 8095    # 添加路由
orca gateway remove webmail                        # 删除路由

orca gateway config --show    # 以 YAML 展示当前网关配置
orca gateway config --domain dev.example.com       # 修改基础域名
orca gateway config --tls-mode custom \
  --cert-file wildcard.pem --key-file wildcard-key.pem  # 使用自定义证书
```

#### 证书机构（CA）

```bash
orca ca info                  # 查看 CA 主题、有效期、SHA-256 指纹
orca ca export > orca-ca.pem  # 导出 CA 证书 PEM 到标准输出
orca ca install               # 安装 CA 到系统信任库（需要 sudo）
```

`ca install` 自动执行平台相关命令：
- **macOS**：通过 `security add-trusted-cert` 加入系统钥匙串
- **Windows**：通过 `certutil -addstore` 加入 ROOT 存储
- **Linux**：复制到 `/usr/local/share/ca-certificates/` 并执行 `update-ca-certificates`

#### 部署

```bash
orca deploy ./my-project           # 从目录部署堆栈（读取 orca.yaml）
orca deploy --template wordpress   # 从模板目录部署模板
```

#### 模板

```bash
orca templates list                # 列出所有可用模板
orca templates search database     # 按名称、描述或分类搜索
```

#### 配置

```bash
orca config export > team-config.yaml     # 以 YAML 导出配置（不含机密）
orca config export --include-secrets      # 包含 API 密钥、token、证书 PEM
orca config import team-config.yaml       # 从 YAML 导入并合并配置

orca config get gateway.domain            # 读取某项设置
orca config set gateway.domain localhost  # 更新某项设置
```

#### 版本

```bash
orca version    # 显示 CLI 与守护进程版本
```

### 团队协作

Orca 为「每个开发者在本地跑同一套环境」的团队而设计。

#### `orca.yaml` —— 项目级配置

在项目仓库中、`docker-compose.yml` 旁添加 `orca.yaml`，声明网关路由与环境链接：

```yaml
# orca.yaml —— 提交到 git，团队共享
gateway:
  - hostname: app
    service: frontend
    port: 3000
  - hostname: api
    service: backend
    port: 8080

links:
  Frontend:
    - name: Web App
      local: app
      staging: https://staging.example.com
      production: https://www.example.com

  Backend:
    - name: API
      local: api
      staging: https://staging-api.example.com
      production: https://api.example.com
    - name: API Docs
      local: api/docs
```

当任何团队成员通过 Orca 部署该堆栈时：
- 网关路由自动注册（`https://app.localhost`、`https://api.localhost`）
- 环境链接出现在网关面板中，带 本地 / 预发 / 生产 标签
- `local` 值引用网关主机名——自动解析为完整 URL
- 其他环境为直链（不代理）

#### 自定义团队域名

如果团队使用共享域名（如 `*.dev.example.com`，DNS 指向 `127.0.0.1`），一次性配置：

```bash
orca gateway config \
  --domain dev.example.com \
  --tls-mode custom \
  --cert-file wildcard.pem \
  --key-file wildcard-key.pem
orca gateway start
```

之后所有项目的 `orca.yaml` 路由都使用团队域名：`https://app.dev.example.com`。

#### 新成员入职

建立配置仓库，存放团队网关配置、泛域名证书和初始化脚本：

```bash
#!/bin/bash
# setup.sh —— 新成员只需执行一次
orca config import team-config.yaml
orca gateway start
echo "搞定！部署任何带 orca.yaml 的项目即可开始。"
```

此后每个部署的项目都会自动带上团队域名、路由和环境链接。

### 仪表盘

- **资源历史图表**——CPU 与内存时间序列，悬停显示数值
- **CPU/内存占用排行**——每个容器附带迷你图表
- 容器、镜像、堆栈数量与 GPU 状态一目了然
- **资源用量告警**——内存超 90% 或 CPU 持续 90% 时弹窗提醒
- **系统清理**——清理容器、镜像、卷、网络与构建缓存

### 容器备份与导出

- **导出容器**为 tar 文件（容器文件系统）
- **保存镜像**为 tar 文件（含全部镜像层）
- 文件保存对话框选择目标位置
- 通过守护进程 API 同时支持本地与远程主机

### 定时容器操作

- **内置 cron 调度器**——按计划重启、停止或启动容器
- 标准 cron 表达式与常用预设
- 每条计划可单独启用/禁用
- 由守护进程执行——桌面应用关闭后依然生效
- 在 `设置 → 定时任务` 页签管理

### 环境管理

- 首次启动的**欢迎向导**——引导新用户完成运行时设置
- 跨平台自动检测 Docker/Podman 安装
- **进度对话框**显示分步输出的一键安装
- 健康检查，带修复按钮与详细诊断
- 与现有 Docker 安装共存

### 远程端口转发

- **WebSocket TCP 隧道**——访问远程主机上的任意服务，如同本地
- 在 K8s 服务上点击「端口转发」→ `localhost:8080` 即连接到远程服务
- 穿透任意防火墙/NAT——复用既有的已认证 HTTPS 连接
- 无需 VPN、SSH 或额外工具——只要有已安装的 Orca 守护进程
- 支持多个并发隧道
- 本地与远程主机使用相同 UI

```
你的浏览器 → localhost:8080 → [WebSocket 隧道] → 远程守护进程 → K8s 服务:80
```

### 自动部署（GitHub Webhook）

- **推送即部署**——推送代码到 GitHub，容器自动更新
- GitHub Actions 构建镜像 → 推送到 ghcr.io → Webhook → 守护进程拉取并重新部署
- **标签过滤**——按 `v*`（版本标签）、`latest`、`main` 或 `*`（任意推送）部署
- **容器定向**——按名称重部署指定容器，或按镜像自动匹配
- **配置保留**——端口、卷、环境变量、标签、重启策略全部保留
- **部署历史**——带时间戳的成功/失败日志
- **HMAC-SHA256 签名校验**——拒绝未签名/被篡改的 Webhook
- **Docker Hub 支持**——同样适配 Docker Hub 的 Webhook

```
git push → GitHub Actions → ghcr.io → Webhook → Orca 守护进程 → 拉取 + 重新部署
```

### 安全

- **强制 API token 认证**——首次运行自动生成，每个请求都要校验
- 恒定时间 token 比较（防时序攻击）
- 健康检查端点是唯一的免认证路由
- 支持 Unix socket 模式与文件权限（生产环境推荐）
- 绑定非 localhost 地址时给出网络暴露警告

### 桌面应用

- 自定义标题栏，显示运行时状态与版本
- **系统托盘**——关闭到托盘而非退出
- **自动更新**——带签名校验与无缝守护进程重启
- 通知铃铛与活动流
- **命令面板**（Ctrl+K）——模糊搜索页面、资源与操作
- **键盘快捷键**——按 `?` 查看全部，`Ctrl+R` 刷新
- **网络拓扑**——网络与容器连接的可视化图
- 带操作按钮的 Toast 通知
- 深色玻璃拟态主题与流畅动画

### 跨平台

- **Linux**：原生 Docker/Podman——无需虚拟机
- **macOS**：基于 Apple Virtualization.framework 的 Lima 虚拟机、VirtioFS、代理透传
- **Windows**：WSL2 + Docker，自动配置 TCP 桥接
- 全平台带签名的自动更新
- 实时流式进度的引导式安装向导

## macOS 与 Lima：工作原理

在 macOS 上，Docker 运行在由 [Lima](https://lima-vm.io) 管理的轻量 Linux 虚拟机里。Orca 会自动完成全部设置——不需要 Docker Desktop、OrbStack 或其他商业工具。

### Orca 安装了什么

在 macOS 上首次启动时，安装向导会（通过 Homebrew）安装：
- **Lima**——使用 Apple Virtualization.framework 的轻量虚拟机管理器
- **Docker CLI** + **Docker Compose** + **Docker Buildx**——标准 Docker 工具链
- 名为 "orca" 的 Linux 虚拟机——自动分配内存、4 核 CPU、VirtioFS 挂载与端口转发
- **HWE 内核（6.17）**——从 Ubuntu 默认的 6.8 升级，完整支持 VirtioFS 权限

### 端口转发

容器端口自动转发回 Mac。运行 `docker run -p 8080:80 nginx` 后即可在 `http://localhost:8080` 访问——和 Docker Desktop 一样。

### Bind mount 权限

**Bind mount 权限开箱即用。** Orca 在 Lima 虚拟机中提供新一代 Linux 内核（6.17），解决了旧内核上困扰 VirtioFS 的权限问题。使用 HWE 内核后：

- **chmod/chown** 对挂载的宿主机目录生效
- **root 与非 root 容器**都可读写 bind mount
- 修改权限的**入口脚本**能正常运行
- 无需 `--user` 参数，无需 `PUID`/`PGID` 环境变量，无需任何变通

这使 Orca 拥有与 Docker Desktop 相同的 bind mount 行为——但不需要 Docker Desktop 的专有文件系统层。

### 自动收敛

每次启动时，Orca 守护进程都会检查 Lima 虚拟机配置并自动补齐缺失设置（端口转发、挂载、内核配置）。升级 Orca 后虚拟机会自动修补——无需手动重建。

## 截图

<details>
<summary>点击展开</summary>

| | |
|---|---|
| ![Containers](screenshots/02-containers.png) | ![Container Detail](screenshots/03-container-detail.png) |
| **容器**——Compose 堆栈、实时 CPU/内存 | **容器详情**——概览、日志、终端、文件 |
| ![Images](screenshots/06-images.png) | ![Kubernetes](screenshots/10-kubernetes.png) |
| **镜像**——拉取、构建、扫描、标签、镜像层 | **Kubernetes**——Pods、Deployments、Services、Helm |
| ![Network Topology](screenshots/09-network-topology.png) | ![App Catalog](screenshots/16-app-catalog.png) |
| **网络拓扑**——可视化网络图 | **应用目录**——一键模板 |
| ![Settings AI](screenshots/20-settings-ai.png) | ![System Health](screenshots/17-system-health.png) |
| **AI 与 Agent**——5 家提供商、MCP 服务器 | **系统健康**——诊断与设置 |

</details>

## 架构

```
┌─────────────────────────────────────────────────────────┐
│              Orca Desktop (GUI)                         │
│              SolidJS + TypeScript                       │
│         主机选择器：本地 | 远程服务器                    │
└───────────────┬─────────────────────┬───────────────────┘
                │                     │
        ┌───────▼───────┐     ┌───────▼───────┐
        │  本地守护进程  │     │  远程守护进程  │ ← apt install orca-daemon
        │  (端口 9477)  │     │ (HTTPS/9477)  │
        ├───────────────┤     ├────────────────┤
        │    平台层     │     │     Linux     │
        │  Linux/macOS/ │     │     Docker    │
        │   Windows     │     │    (原生)     │
        ├───────────────┤     ├────────────────┤
        │ Docker/Podman │     │ Docker/Podman │
        └───────────────┘     └───────────────┘
```

守护进程通过标准 API（bollard）与 Docker/Podman 通信。macOS 上管理 Lima 虚拟机，Windows 上管理 WSL2 发行版，Linux 上直接对接运行时——无需虚拟机。远程守护进程通过 HTTPS + Bearer token 认证管理。

## 快速开始

### 安装与运行

下载并启动 Orca Desktop 即可——其余交给它：

1. 从 [Releases](https://github.com/edvin/orca/releases) **下载**
2. **运行**安装程序（Windows：exe/msi，macOS：dmg，Linux：AppImage/deb）
3. **Orca Desktop 会检查你的环境**并自动补齐缺失组件：

| 平台 | Orca Desktop 为你配置 |
|------|----------------------|
| **Linux** | 未检测到时安装 Docker 或 Podman |
| **macOS** | 安装 Homebrew → Lima → 创建带 Docker 的 Linux 虚拟机 |
| **Windows** | 启用 WSL2 → 安装 Ubuntu → 在其中安装 Docker |

无需手动配置。「环境」页面会通过一键修复按钮引导你完成所有必要步骤。

### 管理远程服务器

在任意 Linux 服务器上安装 Orca 守护进程，然后在桌面端管理：

```bash
# 一行命令安装（Ubuntu/Debian）
curl -1sLf 'https://dl.cloudsmith.io/public/edvin/orca/setup.deb.sh' | sudo bash
sudo apt install orca-daemon
```

这会把守护进程安装为 systemd 服务：
- **开机自启**，崩溃自动重启
- 在 `/etc/orca/config.json` **生成 API token**
- 安装完成后**输出连接信息**（URL + token）

然后在 Orca Desktop：**设置 → 远程主机 → 添加主机**——粘贴 URL 与 token。

**生产环境 TLS：**在守护进程前放一个反向代理：

```bash
# Caddy（自动 TLS）
sudo apt install caddy
echo 'orca.example.com { reverse_proxy localhost:9477 }' | sudo tee /etc/caddy/Caddyfile
sudo systemctl restart caddy
```

完整配置见 [deploy/caddy-example](deploy/caddy-example) 与 [deploy/nginx-example](deploy/nginx-example)。完整指南见 [docs/remote-management.md](docs/remote-management.md)。

**更新：**标准 apt——`sudo apt update && sudo apt upgrade`

**管理命令：**
```bash
systemctl status orca-daemon    # 查看状态
journalctl -u orca-daemon -f    # 查看日志
cat /etc/orca/config.json       # 查看 API token
```

### 运行守护进程（开发）

```bash
# 克隆并构建
git clone https://github.com/edvin/orca.git
cd orca
cargo build --release --bin orca-daemon

# 运行（开发用 TCP 模式）
./target/release/orca-daemon

# 或使用 Unix socket
./target/release/orca-daemon --socket auto
```

守护进程默认监听 `http://127.0.0.1:9477`。首次运行会生成 API token 并写入配置文件，位置因平台而异：

| 平台 | 配置路径 |
|------|----------|
| Linux（用户/开发守护进程） | `~/.config/orca/config.json` |
| Linux（apt/systemd 守护进程） | `/etc/orca/config.json` |
| macOS | `~/Library/Application Support/orca/config.json` |
| Windows | `%APPDATA%\orca\config.json` |

获取 token 最简单的方式是桌面应用：**设置 → Agent 集成**，API Token 栏带显隐与复制按钮。

### 配置 AI（可选）

为内置 AI 助手设置 API 密钥：

```bash
# 方式一：环境变量
export ANTHROPIC_API_KEY="sk-ant-..."
# 或
export OPENAI_API_KEY="sk-..."

# 方式二：在 GUI 中配置
# 打开 设置 → AI 助手 → 填入密钥并选择提供商
```

### 用 curl 测试

```bash
# 健康检查（无需认证）
curl http://127.0.0.1:9477/api/v1/health

# 读取 API token（macOS：~/Library/Application Support/orca/config.json）
TOKEN=$(cat ~/.config/orca/config.json | grep api_token | cut -d'"' -f4)

# 列出容器（需认证）
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:9477/api/v1/containers

# 列出镜像
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:9477/api/v1/images

# 列出 Compose 堆栈
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:9477/api/v1/stacks

# 容器资源统计
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:9477/api/v1/containers/<id>/stats

# 在容器内执行命令
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  http://127.0.0.1:9477/api/v1/containers/<id>/exec \
  -d '{"command": ["uname", "-a"]}'
```

### 运行 GUI

```bash
# 安装前端依赖
cd gui && npm install && cd ..

# 开发模式（需先运行守护进程）
cargo tauri dev

# 生产构建
cargo tauri build
```

### CLI

```bash
cargo build --release --bin orca

# 检查守护进程状态
./target/release/orca status

# 主机管理
./target/release/orca machine list
```

## Agent 集成

Orca Desktop 暴露了面向 Agent 的 API，AI 工具可直接管理你的容器。

### Claude Code / Claude Desktop（MCP）

把以下内容加入 MCP 配置文件：

```json
{
  "mcpServers": {
    "orca": {
      "url": "http://127.0.0.1:9477/api/v1/agent/mcp",
      "headers": {
        "Authorization": "Bearer YOUR_TOKEN_HERE"
      }
    }
  }
}
```

将 `YOUR_TOKEN_HERE` 替换为你的 API token。最简单的方式是使用桌面应用：**设置 → Agent 集成**会展示完整配置，并带有显隐/复制按钮，直接复制即可自动代入真实 token。如需从磁盘读取，配置文件路径因平台而异（参见[运行守护进程](#运行守护进程开发)下的表格）。

### OpenAI 兼容 Agent

可在任何支持函数调用的 Agent 框架中使用 OpenAI 兼容端点：

```
Endpoint: http://127.0.0.1:9477/api/v1/agent/openai/chat/completions
Authorization: Bearer YOUR_TOKEN_HERE
```

### 直接调用工具

自定义集成可直接调用工具：

```bash
# 列出可用工具
curl -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:9477/api/v1/agent/tools

# 执行工具
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  http://127.0.0.1:9477/api/v1/agent/execute \
  -d '{"tool": "list_containers", "args": {}}'
```

### 可用工具（43 个工具，8 大类）

| 分类 | 工具 |
|------|------|
| 容器 | list, inspect, start, stop, restart, remove, logs, exec, stats |
| 镜像 | list, pull, remove, prune |
| Compose | 列出堆栈, up, down, pull |
| Kubernetes | status, pods, deployments, services, ingresses, events, configmaps, secrets, scale, restart, delete pod, get yaml, helm list, namespaces |
| 卷 | list, create, remove |
| 网络 | list, create, remove |
| 系统 | health, 环境状态 |
| 诊断 | 诊断容器（inspect + 日志 + stats 合一） |

## 项目结构

```
orca/
├── crates/
│   ├── orca-core/              # Trait 抽象与类型
│   ├── orca-backend-common/    # 共享 bollard + k3s 实现
│   ├── orca-backend-native/    # Linux：直接对接 Docker/Podman
│   ├── orca-backend-macos/     # macOS：Lima 虚拟机管理
│   ├── orca-backend-windows/   # Windows：WSL2 管理
│   ├── orca-daemon/            # REST API 服务（axum）
│   └── orca-cli/               # 命令行工具
├── src-tauri/                  # Tauri 桌面应用外壳
├── gui/                        # SolidJS 前端
│   └── src/
│       ├── pages/              # Stacks、Containers、Images、Volumes、
│       │                       # Networks、Kubernetes、Machine、Settings
│       ├── components/         # LogViewer、ExecTerminal、Toast、
│       │                       # RunContainerDialog、AiAssistant、Sidebar
│       └── lib/                # 类型、格式化工具、事件系统、i18n
└── .github/workflows/          # CI/CD（Linux、macOS、Windows）
```

## 技术栈

| 层级 | 技术 |
|------|------|
| 桌面外壳 | Tauri 2 |
| 前端 | SolidJS + TypeScript |
| 守护进程 | Rust + Axum |
| 容器 API | Bollard（Docker 兼容） |
| Kubernetes | kube-rs + k3s |
| AI | Anthropic Claude / OpenAI GPT（用户可选） |
| 虚拟机（macOS） | Lima（Apple Virtualization.framework） |
| 虚拟机（Windows） | WSL2 |

## API 参考

守护进程在 `http://127.0.0.1:9477/api/v1/` 暴露 REST API：

| 端点 | 方法 | 说明 |
|------|------|------|
| `/health` | GET | 守护进程健康检查（免认证） |
| `/events` | GET | SSE 事件流 |
| `/containers` | GET, POST | 列出 / 创建容器 |
| `/containers/:id` | GET, DELETE | 查看 / 删除 |
| `/containers/:id/start` | POST | 启动容器 |
| `/containers/:id/stop` | POST | 停止容器 |
| `/containers/:id/restart` | POST | 重启容器 |
| `/containers/:id/stats` | GET | 实时资源统计 |
| `/containers/:id/logs` | GET | SSE 日志流 |
| `/containers/:id/exec` | POST | 执行命令 |
| `/containers/:id/export/run` | GET | 导出为 docker run |
| `/containers/:id/export/compose` | GET | 导出为 docker-compose.yml |
| `/images` | GET | 列出镜像 |
| `/images/:id` | GET | 查看镜像 |
| `/images/pull` | POST | 拉取镜像（SSE 进度） |
| `/images/build` | POST | 构建镜像（SSE 日志） |
| `/images/search` | GET | 搜索 Docker Hub |
| `/images/prune` | POST | 清理未使用镜像 |
| `/images/batch-delete` | POST | 批量删除镜像 |
| `/volumes` | GET, POST | 列出 / 创建卷 |
| `/volumes/:name` | DELETE | 删除卷 |
| `/networks` | GET, POST | 列出 / 创建网络 |
| `/networks/:name` | DELETE | 删除网络 |
| `/registries` | GET, POST | 列出 / 添加镜像仓库 |
| `/registries/:server` | DELETE | 删除镜像仓库 |
| `/stacks` | GET | 列出 Compose 堆栈 |
| `/stacks/:name/up` | POST | docker compose up |
| `/stacks/:name/down` | POST | docker compose down |
| `/stacks/:name/pull` | POST | docker compose pull |
| `/stacks/:name/start` | POST | 启动堆栈服务 |
| `/stacks/:name/stop` | POST | 停止堆栈服务 |
| `/stacks/:name/restart` | POST | 重启堆栈服务 |
| `/machines` | GET | 列出主机 |
| `/k8s/status` | GET | Kubernetes 集群状态 |
| `/k8s/enable` | POST | 启用 Kubernetes |
| `/k8s/disable` | POST | 禁用 Kubernetes |
| `/k8s/kubeconfig` | GET | 导出 kubeconfig |
| `/k8s/namespaces` | GET | 列出命名空间 |
| `/k8s/pods/:ns` | GET | 列出 Pod |
| `/k8s/deployments/:ns` | GET | 列出 Deployment |
| `/k8s/services/:ns` | GET | 列出 Service |
| `/k8s/ingresses/:ns` | GET | 列出 Ingress |
| `/k8s/pvcs/:ns` | GET | 列出 PVC |
| `/k8s/pvs` | GET | 列出 PV |
| `/k8s/apply` | POST | 应用 YAML 清单 |
| `/templates` | GET | 列出应用模板 |
| `/templates/user` | POST, DELETE | 创建/更新、删除用户模板 |
| `/templates/:id/deploy` | POST | 部署模板 |
| `/stacks/:name/env` | PATCH | 更新堆栈 .env 文件中的环境变量 |
| `/ca/certificate` | GET | 下载 CA 证书（免认证） |
| `/ca/info` | GET | CA 信息（主题、有效期、指纹） |
| `/gateway/status` | GET | 网关运行状态与配置 |
| `/gateway/start` | POST | 启动 Caddy 网关容器 |
| `/gateway/stop` | POST | 停止网关容器 |
| `/gateway/routes` | GET, POST | 列出 / 添加网关路由 |
| `/gateway/routes/:hostname` | PUT, DELETE | 更新 / 删除路由 |
| `/gateway/config` | GET, PUT | 获取 / 更新网关设置 |
| `/environment/status` | GET | 环境健康检查 |
| `/environment/fix` | POST | 执行修复动作 |
| `/system/health` | GET | 系统健康总览 |
| `/ai/ask` | POST | AI 助手查询 |
| `/settings/ai` | GET, POST | 获取 / 更新 AI 设置 |
| `/agent/tools` | GET | 列出 Agent 工具 |
| `/agent/execute` | POST | 执行 Agent 工具 |
| `/agent/openai/chat/completions` | POST | OpenAI 兼容端点 |
| `/agent/mcp` | POST | MCP 服务器端点 |

完整 API 见 [`crates/orca-daemon/src/api.rs`](crates/orca-daemon/src/api.rs)。

## 发布

发布流程完全自动化。发布新版本：

```bash
# 1. 更新 tauri.conf.json 与 Cargo.toml 中的版本号
# 2. 提交版本变更
git add -A && git commit -m "Release v0.2.0"

# 3. 打标签并推送
git tag v0.2.0
git push && git push --tags
```

这会触发发布工作流：

1. 创建带自动生成 release notes 的草稿 GitHub Release
2. 并行构建 **Linux**（AppImage、deb）、**macOS**（dmg）、**Windows**（exe、msi）的签名 Tauri 应用
3. 将守护进程二进制打包为应用内 sidecar
4. 用项目签名密钥为所有更新工件签名
5. 上传 `latest.json` 供 Tauri 自动更新器使用
6. 发布 Release

**自动更新：**已安装 Orca Desktop 的用户会自动收到更新通知。应用启动时检查 `https://github.com/edvin/orca/releases/latest/download/latest.json`，带签名校验地下载并安装更新。

### 发布工件

| 平台 | 安装包 | 自动更新 |
|------|--------|----------|
| Linux | `.AppImage`、`.deb` | AppImage 自更新 |
| macOS | `.dmg` | 应用包自更新 |
| Windows | `.exe`（NSIS）、`.msi` | Exe 自更新 |

## 参与贡献

欢迎贡献！（本 fork 与上游一致）请先开 issue 讨论你想要修改的内容。

开发环境配置与规范见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 许可证

[MIT](LICENSE)

---

<p align="center">
  中文文档由 <a href="https://github.com/F2xuan">F2xuan</a> 维护 · 基于 <a href="https://github.com/edvin/orca">edvin/orca</a> · <a href="./README.md">English README</a>
</p>
