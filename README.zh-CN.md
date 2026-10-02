<div align="center">

# Mira

**一个足够简单的、支持记忆的开源大模型聊天客户端**

仿照 ChatGPT 设计 · 本地优先 · 轻量 · 易于改造

[功能](#功能) · [为什么做](#为什么做-mira) · [快速开始](#快速开始) · [技术栈](#技术栈) · [隐私](#隐私) · [文档](#文档) · [贡献](#欢迎-fork)

简体中文 · **[English](README.md)**

</div>

---

## 为什么做 Mira

我需要一个**足够简单**的开源大模型聊天软件，并且支持记忆功能。

市面上的方案要么太重，要么把模型和记忆绑死在某个云服务上，要么代码复杂到改不动。于是我仿照 ChatGPT 的设计写了 Mira——**轻量、支持自定义模型、易于自己修改**。

如果你也想要一个属于自己的、能记住你说过什么的大模型聊天应用，欢迎 fork 出去，改成最适合你自己的样子。

## 功能

- **聊天** — ChatGPT 风格的对话界面，支持 Markdown 渲染和代码高亮
- **长期记忆** — 自动从对话中提炼记忆，跨对话注入相关上下文；也支持手动保存记忆
- **多 Provider 配置** — 任意 OpenAI-compatible 接口（OpenAI、DeepSeek、Ollama、自建网关……），API Key 存入系统凭据库
- **项目组织** — 用项目归类对话，项目内对话共享上下文
- **本地存储** — 对话、项目、记忆都存在本地 SQLite，API Key 存在系统凭据库。聊天内容只会发送给你自己配置的服务商——详见[隐私说明](#隐私)
- **中英双语** — 界面支持中英文切换，默认英文

## 截图

![Mira 聊天界面](screenshots/chat.png)

## 快速开始

### 下载安装

前往 [Releases 页面](https://github.com/MrSibe/Mira/releases) 下载对应平台的安装包：

| 平台                       | 文件                                                           |
| -------------------------- | -------------------------------------------------------------- |
| **Windows**                | `Mira_<版本>_x64-setup.msi.zip` 或 `Mira_<版本>_x64_en-US.msi` |
| **macOS（Intel）**         | `Mira_<版本>_x64.dmg`                                          |
| **macOS（Apple Silicon）** | `Mira_<版本>_aarch64.dmg`                                      |
| **Linux**                  | `Mira_<版本>_amd64.deb` 或 `Mira_<版本>_amd64.AppImage`        |

下载后直接安装即可使用，**无需任何开发环境**。

> 首次打开 Mira 时会提示配置 API Provider。  
> 你需要一个 **OpenAI-compatible 的 API Key** 才能开始聊天——可以是 OpenAI、DeepSeek 或其他兼容服务商。

### 自行构建

如果你希望从源码构建（需要 [Node.js](https://nodejs.org/) 22+、[pnpm](https://pnpm.io/) 11+、[Rust](https://www.rust-lang.org/) stable）：

```bash
pnpm install
pnpm tauri build
```

构建产物在 `src-tauri/target/release/bundle/`。

## 技术栈

| 层       | 技术                                          |
| -------- | --------------------------------------------- |
| 前端     | React 19 · TypeScript · TailwindCSS · Zustand |
| 桌面框架 | Tauri 2                                       |
| 后端     | Rust 原生服务 · Pi AI runtime sidecar         |
| 存储     | SQLite（本地文件）                            |
| 凭据     | 系统凭据库（keyring）                         |
| 国际化   | 轻量自建 i18n（中/英）                        |

## 隐私

Mira 自己没有任何后端服务，也不会把数据上传到 Mira 服务器。但“本地”并不等于“永不离开本机”：为了获得回复，Mira 会通过你配置的 OpenAI-compatible 服务商发送当前消息、随消息一起发送的对话历史、System Prompt，以及检索到的记忆或项目上下文；后台记忆流程还可能把最近一轮对话发给你配置的后台模型。

这些数据的持久化记录保存在本地 SQLite，但选中的内容可能包含在上述请求中。API Key 存储在系统凭据库，并用于向服务商认证。更新检查和下载还会访问配置的 GitHub updater/release 地址。详见 [docs/security.md](docs/security.md)；本地数据库备份依然包含私人数据。

## 架构

下图是**当前**架构。完整说明（包括已落地设计和已批准但尚未实现的目标方向）见 [docs/architecture.md](docs/architecture.md)。

```txt
React UI / Zustand
  ↓ invoke / events
Tauri Commands（Rust 应用协调）
  ├── Model adapter → 独立 Pi AI runtime → 配置的服务商
  ├── 记忆提取 / 检索
  ├── SQLite（持久化用户数据）
  └── OS keyring（凭据）
```

## 目录结构

```txt
src
├── components      # UI 组件
├── pages           # 聊天页 / 设置页
├── store           # Zustand 状态管理
├── core            # Tauri 客户端 & 类型
├── i18n            # 国际化（en / zh）
└── utils           # 工具函数

src-tauri/src
├── chat.rs         # Tauri 命令入口
├── database.rs     # SQLite 数据层
├── memory.rs       # 记忆提炼 & 注入
├── model.rs        # Mira 上下文构建 / runtime adapter
├── runtime.rs      # 私有 JSONL 进程桥接
├── secrets.rs      # 系统凭据库读写
└── types.rs        # 共享类型

runtime            # Pi AI 推理、独立二进制构建与离线测试
```

第一阶段保留 OpenAI-compatible 设置，不启用原生 Provider 目录、OAuth 或 Tools。
Tauri dev/build 会自动使用锁定版本的 Bun 编译 sidecar（由 pnpm 安装，最终用户无需安装）。
单独执行 Rust 检查前请先运行 `pnpm runtime:build`；`pnpm runtime:test` 验证编译后的 runtime。
进程边界与二进制体积代价见 [ADR 0006](docs/adr/0006-pi-ai-sidecar.md)。

## 文档

- [产品定义](PRODUCT.md) — Mira 是什么、不是什么
- [架构说明](docs/architecture.md) — 当前设计与目标方向
- [设计契约](DESIGN.md) — 令牌、布局、无障碍、错误与取消约定
- [工程与运维](docs/engineering.md) — 构建、CI，以及尚未实现的部分
- [测试说明](docs/testing.md) — 单元、SQLite 集成和 mock UI smoke
- [记忆系统](docs/memory-system.md)
- [项目上下文](docs/project-context.md)
- [安全说明](docs/security.md)
- [架构决策记录（ADR）](docs/adr/)
- [Agent / 贡献者指南](AGENTS.md)

## 范围边界

v1 只做本机单用户、纯聊天、长期记忆、SQLite 本地存储、多 Provider 配置。

**不做：** RAG、向量数据库、工具调用、多用户、云同步。

## 欢迎 fork

Mira 的代码刻意保持简单和可读。如果你想要一个属于自己的大模型聊天应用，fork 它，然后：

- 换成你喜欢的 UI 风格
- 接入你自己的模型或网关
- 调整记忆策略
- 加上你想要的功能

PR 欢迎，但请先开 issue 讨论改动方向。

## License

[GPL-3.0](LICENSE)
