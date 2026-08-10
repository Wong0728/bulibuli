/**
 * 生成UUID的兼容性函数
 * 支持Docker环境和旧浏览器，当crypto.randomUUID不可用时使用fallback实现
 */
export function generateUUID(): string {
	// 优先使用原生的crypto.randomUUID
	if (typeof crypto !== 'undefined' && crypto.randomUUID) {
		return crypto.randomUUID();
	}

	// Fallback实现：生成符合UUID v4格式的随机字符串
	return 'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, function (c) {
		const r = (Math.random() * 16) | 0;
		const v = c === 'x' ? r : (r & 0x3) | 0x8;
		return v.toString(16);
	});
}

/**
 * 生成短UUID（去掉连字符）
 */
export function generateShortUUID(): string {
	return generateUUID().replace(/-/g, '');
}
