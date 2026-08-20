import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import path from 'node:path';

// 产物落位到 `static/app/`，由 Rust 后端以 /app/* 路径 serve。
// 入口固定为 `static/app/index.html`，与现有 static/index.html 共存，方便灰度切换。
export default defineConfig({
  root: path.resolve(__dirname),
  base: '/app/',
  plugins: [vue()],
  build: {
    outDir: path.resolve(__dirname, '../static/app'),
    emptyOutDir: true,
    assetsDir: 'assets',
    sourcemap: false,
    target: 'es2020',
    rollupOptions: {
      output: {
        // 给 asset 文件名加 hash，方便 Rust 端设置缓存
        entryFileNames: 'assets/[name]-[hash].js',
        chunkFileNames: 'assets/[name]-[hash].js',
        assetFileNames: 'assets/[name]-[hash].[ext]'
      }
    }
  },
  server: {
    port: 5173,
    // dev 阶段保留当前页面的所有路由（包括 /api/* 反向代理到 Rust 服务的 8080）
    proxy: {
      '/api': 'http://127.0.0.1:8080',
      '/socket.io': { target: 'http://127.0.0.1:8080', ws: true }
    }
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, 'src')
    }
  }
});