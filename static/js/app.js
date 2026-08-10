// 注意：这里不能带 ?v= 查询串——其余模块之间互相 import 时用的是不带查询串的
// './core.js' 等路径，若入口带查询串，浏览器会把同一文件当成两个不同模块并各执行
// 一遍（事件重复绑定、双 WebSocket、双轮询）。缓存版本控制统一由 index.html 的
// <script src="js/app.js?v=N"> 承担。
import './core.js';
import './bootstrap.js';
