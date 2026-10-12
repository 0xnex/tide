import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { nodePolyfills } from "vite-plugin-node-polyfills";

// Anchor 和 web3.js 在浏览器里需要 Buffer 等 Node 内置模块
export default defineConfig({
  plugins: [react(), nodePolyfills({ include: ["buffer", "crypto", "stream", "util"], globals: { Buffer: true } })],
});
