# MCP HTTP smoke test for NeoWaves
# Usage: .\commands\mcp_smoke.ps1 -Addr 127.0.0.1:7464 -ToolName get_debug_summary

param(
    [string]$Addr = "127.0.0.1:7464",
    [string]$ToolName = "get_debug_summary",
    [string]$ToolArgsJson = ""
)

$ErrorActionPreference = "Stop"

$base = "http://$Addr"
$rpc = "$base/rpc"

function Invoke-McpRpc {
    param(
        [Parameter(Mandatory = $true)][int]$Id,
        [Parameter(Mandatory = $true)][string]$Method,
        [Parameter(Mandatory = $true)]$Params
    )
    $payload = @{
        jsonrpc = "2.0"
        id      = $Id
        method  = $Method
        params  = $Params
    }
    $body = $payload | ConvertTo-Json -Depth 10 -Compress
    Invoke-RestMethod -Uri $rpc -Method Post -ContentType "application/json" -Body $body
}

Write-Host "== MCP Smoke Test =="
Write-Host "Addr: $Addr"

try {
    $health = Invoke-RestMethod -Uri "$base/health" -Method Get
    Write-Host "Health: $health"
} catch {
    Write-Error "Health check failed: $($_.Exception.Message)"
    exit 1
}

Write-Host ""
Write-Host "1) list_tools"
$list = Invoke-McpRpc -Id 1 -Method "list_tools" -Params @{}
if ($list.error) {
    Write-Error "list_tools error: $($list.error.message)"
    exit 1
}
Write-Host ("tools: " + ($list.result.tools | ForEach-Object { $_.name } | Sort-Object | Out-String).Trim())

Write-Host ""
Write-Host "2) call_tool: $ToolName"
$toolArgs = @{}
if ($ToolArgsJson -and $ToolArgsJson.Trim() -ne "") {
    $toolArgs = $ToolArgsJson | ConvertFrom-Json
}
$call = Invoke-McpRpc -Id 2 -Method "call_tool" -Params @{ name = $ToolName; arguments = $toolArgs }
if ($call.error) {
    Write-Error "call_tool error: $($call.error.message)"
    exit 1
}
Write-Host "call_tool ok"
Write-Host ($call.result | ConvertTo-Json -Depth 10)

Write-Host ""
Write-Host "Done."
