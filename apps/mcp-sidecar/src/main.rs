//! ServerDesk MCP sidecar（stdio MCP Server）。
//!
//! M3 实现：通过带 token 鉴权的本地 IPC 桥接到主应用，暴露
//! list_servers / exec_command / server_status / read_file / write_file 工具。
//! 凭据只存在于主应用进程内，sidecar 仅做协议桥接与安全过滤转发。
//! M1 仅骨架：验证 workspace 编译与二进制产出。

fn main() {
    // MCP 走 stdio：任何启动日志都必须写 stderr，避免污染协议通道。
    eprintln!(
        "serverdesk-mcp v{} — M1 骨架，MCP 工具实现将于 M3 提供",
        env!("CARGO_PKG_VERSION")
    );
}
