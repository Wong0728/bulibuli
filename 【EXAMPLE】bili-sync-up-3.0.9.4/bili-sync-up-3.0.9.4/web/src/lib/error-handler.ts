import { toast } from 'svelte-sonner';
import type { ApiError, ClassifiedError } from './types';
import { ErrorType, ErrorTypeMessages } from './types';

/**
 * 错误处理器类，用于统一处理和显示错误信息
 */
export class ErrorHandler {
	/**
	 * 处理API错误并显示用户友好的提示
	 * @param error 错误对象
	 * @param context 错误上下文描述
	 */
	static handleError(error: unknown, context?: string): void {
		console.error('Error occurred:', error, context ? `Context: ${context}` : '');

		let errorInfo: ClassifiedError;

		if (this.isApiError(error)) {
			errorInfo = error;
		} else if (error instanceof Error) {
			errorInfo = {
				error_type: ErrorType.Unknown,
				message: error.message,
				should_retry: false,
				should_ignore: false,
				user_friendly_message: error.message
			};
		} else {
			errorInfo = {
				error_type: ErrorType.Unknown,
				message: '未知错误',
				should_retry: false,
				should_ignore: false,
				user_friendly_message: '发生了未知错误'
			};
		}

		this.showErrorToast(errorInfo, context);
	}

	/**
	 * 显示错误提示Toast
	 * @param error 分类后的错误信息
	 * @param context 错误上下文
	 */
	private static showErrorToast(error: ClassifiedError, context?: string): void {
		const title = context
			? `${context} - ${ErrorTypeMessages[error.error_type]}`
			: ErrorTypeMessages[error.error_type];
		const description = error.user_friendly_message || error.message;

		// 根据错误类型选择不同的提示样式
		switch (error.error_type) {
			case ErrorType.Authentication:
			case ErrorType.Authorization:
				toast.error(title, {
					description:
						description + (error.error_type === ErrorType.Authentication ? ' 请重新登录' : ''),
					duration: 10000,
					action: {
						label: '重新登录',
						onClick: () => {
							// 清除本地存储的认证信息
							localStorage.removeItem('auth_token');
							// 可以添加重定向到登录页面的逻辑
							window.location.reload();
						}
					}
				});
				break;

			case ErrorType.Network:
			case ErrorType.Timeout:
			case ErrorType.ServerError:
				toast.error(title, {
					description: description + (error.should_retry ? ' 可以稍后重试' : ''),
					duration: 8000,
					action: error.should_retry
						? {
								label: '重试',
								onClick: () => {
									// 这里可以添加重试逻辑，具体实现取决于调用上下文
									window.location.reload();
								}
							}
						: undefined
				});
				break;

			case ErrorType.RateLimit:
				toast.warning(title, {
					description: description,
					duration: 6000
				});
				break;

			case ErrorType.NotFound:
			case ErrorType.Permission:
			case ErrorType.FileSystem:
				if (error.should_ignore) {
					toast.warning(title, {
						description: description,
						duration: 4000
					});
				} else {
					toast.error(title, {
						description: description,
						duration: 6000
					});
				}
				break;

			case ErrorType.RiskControl:
				toast.error(title, {
					description: description + ' 请稍后再试或联系管理员',
					duration: 10000
				});
				break;

			case ErrorType.Parse:
			case ErrorType.Configuration:
				toast.error(title, {
					description: description + ' 请联系技术支持',
					duration: 8000
				});
				break;

			default:
				toast.error(title, {
					description: description,
					duration: 6000
				});
		}
	}

	/**
	 * 检查是否为API错误
	 * @param error 错误对象
	 * @returns 是否为API错误
	 */
	private static isApiError(error: unknown): error is ApiError {
		return (
			typeof error === 'object' && error !== null && 'error_type' in error && 'message' in error
		);
	}

	/**
	 * 获取错误的严重程度
	 * @param errorType 错误类型
	 * @returns 严重程度级别
	 */
	static getErrorSeverity(errorType: ErrorType): 'low' | 'medium' | 'high' | 'critical' {
		switch (errorType) {
			case ErrorType.NotFound:
			case ErrorType.Parse:
				return 'low';
			case ErrorType.Network:
			case ErrorType.Timeout:
			case ErrorType.RateLimit:
			case ErrorType.FileSystem:
			case ErrorType.Configuration:
				return 'medium';
			case ErrorType.Permission:
			case ErrorType.ServerError:
			case ErrorType.ClientError:
			case ErrorType.Unknown:
				return 'high';
			case ErrorType.Authentication:
			case ErrorType.Authorization:
			case ErrorType.RiskControl:
				return 'critical';
			default:
				return 'medium';
		}
	}

	/**
	 * 检查错误是否应该上报给监控系统
	 * @param errorType 错误类型
	 * @returns 是否需要上报
	 */
	static shouldReportError(errorType: ErrorType): boolean {
		return [
			ErrorType.ServerError,
			ErrorType.Unknown,
			ErrorType.RiskControl,
			ErrorType.Configuration
		].includes(errorType);
	}

	/**
	 * 获取错误的重试策略
	 * @param error 分类后的错误
	 * @returns 重试策略配置
	 */
	static getRetryStrategy(error: ClassifiedError): {
		shouldRetry: boolean;
		maxRetries: number;
		backoffMs: number;
	} {
		const baseStrategy = {
			shouldRetry: error.should_retry,
			maxRetries: 3,
			backoffMs: 1000
		};

		switch (error.error_type) {
			case ErrorType.Network:
			case ErrorType.Timeout:
				return {
					...baseStrategy,
					maxRetries: 5,
					backoffMs: 2000
				};
			case ErrorType.RateLimit:
				return {
					...baseStrategy,
					maxRetries: 2,
					backoffMs: 5000
				};
			case ErrorType.ServerError:
				return {
					...baseStrategy,
					maxRetries: 3,
					backoffMs: 3000
				};
			default:
				return baseStrategy;
		}
	}
}

/**
 * 装饰器函数，用于自动处理异步函数的错误
 * @param context 错误上下文描述
 */
export function withErrorHandler(context?: string) {
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	return function <T extends (...args: any[]) => Promise<any>>(
		target: unknown,
		propertyKey: string,
		descriptor: TypedPropertyDescriptor<T>
	) {
		const originalMethod = descriptor.value;
		if (!originalMethod) return;

		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		descriptor.value = async function (this: any, ...args: Parameters<T>) {
			try {
				return await originalMethod.apply(this, args);
			} catch (error) {
				ErrorHandler.handleError(error, context || propertyKey);
				throw error; // 重新抛出错误，以便调用者可以处理
			}
		} as T;
	};
}

/**
 * 简化的错误处理函数，用于在组件中快速处理错误
 * @param asyncFunction 异步函数
 * @param context 错误上下文
 * @param showToast 是否显示Toast提示
 * @returns 处理后的异步函数
 */
export function handleAsync<T>(
	asyncFunction: () => Promise<T>,
	context?: string,
	showToast: boolean = true
): () => Promise<T | void> {
	return async () => {
		try {
			return await asyncFunction();
		} catch (error) {
			if (showToast) {
				ErrorHandler.handleError(error, context);
			} else {
				console.error('Error occurred:', error, context ? `Context: ${context}` : '');
			}
		}
	};
}
