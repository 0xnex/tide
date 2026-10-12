import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { nodePolyfills } from "vite-plugin-node-polyfills";

// Anchor 和 web3.js 在浏览器里需要 Buffer 等 Node 内置模块。
// 本地 yarn dev 不运行 Vercel 函数（api/），/api 转发到线上网站的后端（VITE_API_PROXY 可以换）。
export default defineConfig({
  plugins: [react(), nodePolyfills({ include: ["buffer", "crypto", "stream", "util"], globals: { Buffer: true } })],
  server: {
    proxy: {
      "/api": { target: process.env.VITE_API_PROXY ?? "https://app-ten-sable-99.vercel.app", changeOrigin: true },
    },
  },
});
