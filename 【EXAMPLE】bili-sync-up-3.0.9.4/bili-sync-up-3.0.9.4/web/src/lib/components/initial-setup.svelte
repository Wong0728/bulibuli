<script lang="ts">
	import { Input } from './ui/input';
	import { Button } from './ui/button';
	import { Label } from './ui/label';
	import { createEventDispatcher } from 'svelte';
	import api from '$lib/api';
	import { toast } from 'svelte-sonner';
	import * as AlertDialog from './ui/alert-dialog';
	import * as Tabs from './ui/tabs';
	import QrLogin from './qr-login.svelte';
	import type { UserInfo } from '$lib/types';

	const dispatch = createEventDispatcher();

	// 步骤控制
	let currentStep = 1;
	let totalSteps = 2;

	// 第一步：设置 API Token
	let authToken = '';
	let authError = '';
	let isVerifying = false;

	// 第二步：设置 B站凭证
	let sessdata = '';
	let bili_jct = '';
	let buvid3 = '';
	let dedeuserid = '';
	let ac_time_value = '';
	let credentialError = '';
	let isSavingCredential = false;

	// 显示帮助对话框
	let showHelpDialog = false;

	// 组件初始化时清除可能的错误状态
	import { onMount } from 'svelte';

	onMount(() => {
		// 清除可能存在的无效token
		api.setAuthToken('');
		// 重置错误状态
		authError = '';
		credentialError = '';
	});

	// 验证并设置 API Token
	async function setupAuthToken() {
		if (!authToken.trim()) {
			authError = '请输入API Token';
			return;
		}

		isVerifying = true;
		authError = '';

		try {
			// 调用后端API设置Token
			const response = await api.setupAuthToken(authToken);

			if (response.data.success) {
				// 设置本地token
				api.setAuthToken(authToken);

				// Token设置成功，进入下一步
				currentStep = 2;
				toast.success('API Token 设置成功');
			} else {
				authError = response.data.message || 'API Token设置失败';
			}
		} catch (error: unknown) {
			// 清除无效的 Token
			api.setAuthToken('');

			if (error && typeof error === 'object' && 'status' in error && error.status === 401) {
				authError = 'API Token错误，请检查后重试';
			} else {
				authError =
					error instanceof Error ? error.message : '设置失败，请检查网络连接或Token是否正确';
			}
			console.error('Token设置失败:', error);
		} finally {
			isVerifying = false;
		}
	}

	// 保存 B站凭证
	async function saveCredential() {
		// 验证必填字段
		if (!sessdata.trim() || !bili_jct.trim() || !buvid3.trim() || !dedeuserid.trim()) {
			credentialError = '请填写所有必需的凭证信息';
			return;
		}

		isSavingCredential = true;
		credentialError = '';

		try {
			// 调用后端API保存凭证
			const response = await api.updateCredential({
				sessdata: sessdata.trim(),
				bili_jct: bili_jct.trim(),
				buvid3: buvid3.trim(),
				dedeuserid: dedeuserid.trim(),
				ac_time_value: ac_time_value.trim() || undefined
			});

			if (response.data.success) {
				toast.success('B站凭证设置成功');
				// 设置完成，触发完成事件
				dispatch('setup-complete');
			} else {
				credentialError = response.data.message || 'B站凭证保存失败';
			}
		} catch (error: unknown) {
			console.error('保存凭证失败:', error);
			credentialError = error instanceof Error ? error.message : '保存凭证时发生错误';
		} finally {
			isSavingCredential = false;
		}
	}

	// 跳过凭证设置
	function skipCredentialSetup() {
		toast.info('已跳过B站凭证设置，您可以稍后在设置页面中配置');
		dispatch('setup-complete');
	}

	// 返回上一步
	function goBack() {
		if (currentStep > 1) {
			currentStep--;
		}
	}

	// 处理Enter键
	function handleKeyDown(event: KeyboardEvent, action: () => void) {
		if (event.key === 'Enter') {
			action();
		}
	}

	// 处理扫码登录成功
	async function handleQrLoginSuccess(userInfo: UserInfo) {
		// 扫码登录成功后，凭证已经在后端保存
		// 直接触发完成事件
		toast.success(`欢迎，${userInfo.username}！登录成功`);
		dispatch('setup-complete');
	}

	// 处理扫码登录错误
	function handleQrLoginError(error: string) {
		credentialError = error;
		toast.error('扫码登录失败: ' + error);
	}

	// 处理退出登录
	function handleLogout() {
		// 在初始设置页面，用户可以切换账号
		toast.info('请扫码登录新账号');
	}
</script>

<div class="bg-background flex min-h-screen items-center justify-center">
	<div class="w-full max-w-2xl space-y-8">
		<div class="text-center">
			<h1 class="text-foreground mb-2 text-3xl font-bold">欢迎使用 bili-sync</h1>
			<p class="text-muted-foreground">首次使用需要进行初始设置</p>
			<div class="mt-4 flex justify-center">
				<div class="flex items-center space-x-2">
					{#each [...Array(totalSteps).keys()] as i (i)}
						<div
							class="h-2 w-8 rounded-full {currentStep > i
								? 'bg-primary'
								: currentStep === i + 1
									? 'bg-primary/50'
									: 'bg-muted'}"
						></div>
					{/each}
				</div>
			</div>
		</div>

		<div class="bg-card rounded-lg border p-8 shadow-md">
			{#if currentStep === 1}
				<!-- 第一步：设置 API Token -->
				<div class="space-y-6">
					<div class="text-center">
						<h2 class="text-foreground text-xl font-semibold">步骤 1: 设置 API Token</h2>
						<p class="text-muted-foreground mt-2 text-sm">API Token 用于保护管理界面的访问安全</p>
					</div>

					<div class="space-y-4">
						<div>
							<Label for="auth-token" class="text-foreground text-sm font-medium">API Token *</Label
							>
							<Input
								id="auth-token"
								type="password"
								placeholder="请输入您想要设置的API Token"
								bind:value={authToken}
								onkeydown={(e) => handleKeyDown(e, setupAuthToken)}
								class="mt-1 w-full"
								disabled={isVerifying}
							/>
							{#if authError}
								<p class="mt-2 text-sm text-red-600">{authError}</p>
							{/if}
						</div>

						<div class="bg-muted rounded-md p-4">
							<div class="flex">
								<div class="ml-3">
									<h3 class="text-foreground text-sm font-medium">提示</h3>
									<div class="text-muted-foreground mt-2 text-sm">
										<p>• API Token 可以是任意字符串，建议使用复杂的密码</p>
										<p>• 设置后请妥善保管，后续访问管理界面需要使用</p>
										<p>• 如果忘记可以在配置文件中查看或修改</p>
									</div>
								</div>
							</div>
						</div>

						<Button
							onclick={setupAuthToken}
							disabled={isVerifying || !authToken.trim()}
							class="w-full"
						>
							{isVerifying ? '验证中...' : '下一步'}
						</Button>
					</div>
				</div>
			{:else if currentStep === 2}
				<!-- 第二步：设置 B站凭证 -->
				<div class="space-y-6">
					<div class="text-center">
						<h2 class="text-foreground text-xl font-semibold">步骤 2: 设置 B站登录凭证</h2>
						<p class="text-muted-foreground mt-2 text-sm">设置B站登录凭证以启用视频下载功能</p>
					</div>

					<Tabs.Root value="manual" class="w-full">
						<Tabs.List class="grid w-full grid-cols-2">
							<Tabs.Trigger value="manual">手动输入凭证</Tabs.Trigger>
							<Tabs.Trigger value="qr">扫码登录</Tabs.Trigger>
						</Tabs.List>

						<Tabs.Content value="manual" class="space-y-4">
							<div>
								<Label for="sessdata" class="text-foreground text-sm font-medium">SESSDATA *</Label>
								<Input
									id="sessdata"
									type="password"
									placeholder="请输入SESSDATA"
									bind:value={sessdata}
									class="mt-1 w-full"
									disabled={isSavingCredential}
								/>
							</div>

							<div>
								<Label for="bili_jct" class="text-foreground text-sm font-medium">bili_jct *</Label>
								<Input
									id="bili_jct"
									type="password"
									placeholder="请输入bili_jct"
									bind:value={bili_jct}
									class="mt-1 w-full"
									disabled={isSavingCredential}
								/>
							</div>

							<div>
								<Label for="buvid3" class="text-foreground text-sm font-medium">buvid3 *</Label>
								<Input
									id="buvid3"
									type="text"
									placeholder="请输入buvid3"
									bind:value={buvid3}
									class="mt-1 w-full"
									disabled={isSavingCredential}
								/>
							</div>

							<div>
								<Label for="dedeuserid" class="text-foreground text-sm font-medium"
									>DedeUserID *</Label
								>
								<Input
									id="dedeuserid"
									type="text"
									placeholder="请输入DedeUserID"
									bind:value={dedeuserid}
									class="mt-1 w-full"
									disabled={isSavingCredential}
								/>
							</div>

							<div>
								<Label for="ac_time_value" class="text-foreground text-sm font-medium">
									ac_time_value (可选)
								</Label>
								<Input
									id="ac_time_value"
									type="password"
									placeholder="请输入ac_time_value（可选）"
									bind:value={ac_time_value}
									class="mt-1 w-full"
									disabled={isSavingCredential}
								/>
							</div>

							{#if credentialError}
								<p class="text-sm text-red-600">{credentialError}</p>
							{/if}

							<div class="bg-muted rounded-md p-4">
								<div class="flex">
									<div class="ml-3">
										<h3 class="text-foreground text-sm font-medium">获取凭证信息</h3>
										<div class="text-muted-foreground mt-2 text-sm">
											<p>请按以下步骤获取B站登录凭证：</p>
											<ol class="mt-1 list-inside list-decimal space-y-1">
												<li>在浏览器中登录B站</li>
												<li>按F12打开开发者工具</li>
												<li>切换到"网络"或"Network"标签</li>
												<li>刷新页面，找到任意请求</li>
												<li>在请求头中找到Cookie字段</li>
												<li>复制对应的值到上面的输入框中</li>
											</ol>
											<Button
												variant="link"
												onclick={() => (showHelpDialog = true)}
												class="text-primary mt-2 h-auto p-0 underline"
											>
												查看详细教程
											</Button>
										</div>
									</div>
								</div>
							</div>

							<div class="flex space-x-3">
								<Button variant="outline" onclick={goBack} class="flex-1">上一步</Button>
								<Button onclick={saveCredential} disabled={isSavingCredential} class="flex-1">
									{isSavingCredential ? '保存中...' : '完成设置'}
								</Button>
							</div>

							<Button
								variant="ghost"
								onclick={skipCredentialSetup}
								class="text-muted-foreground w-full"
							>
								跳过此步骤（稍后设置）
							</Button>
						</Tabs.Content>

						<Tabs.Content value="qr" class="space-y-4">
							<QrLogin
								onLoginSuccess={handleQrLoginSuccess}
								onLoginError={handleQrLoginError}
								onLogout={handleLogout}
							/>

							<div class="flex space-x-3">
								<Button variant="outline" onclick={goBack} class="flex-1">上一步</Button>
								<Button
									variant="ghost"
									onclick={skipCredentialSetup}
									class="text-muted-foreground flex-1"
								>
									跳过此步骤（稍后设置）
								</Button>
							</div>
						</Tabs.Content>
					</Tabs.Root>
				</div>
			{/if}
		</div>
	</div>
</div>

<!-- 帮助对话框 -->
<AlertDialog.Root bind:open={showHelpDialog}>
	<AlertDialog.Content class="max-w-2xl">
		<AlertDialog.Header>
			<AlertDialog.Title>获取B站登录凭证详细教程</AlertDialog.Title>
		</AlertDialog.Header>
		<div class="space-y-4 text-sm">
			<div>
				<h4 class="font-medium">方法一：通过开发者工具获取</h4>
				<ol class="text-muted-foreground mt-2 list-inside list-decimal space-y-1">
					<li>在Chrome或Edge浏览器中登录B站 (bilibili.com)</li>
					<li>按F12键打开开发者工具</li>
					<li>点击"Network"（网络）标签</li>
					<li>刷新页面（Ctrl+R 或 F5）</li>
					<li>在网络请求列表中点击任意一个请求</li>
					<li>在右侧面板中找到"Request Headers"（请求头）</li>
					<li>找到"Cookie"字段，复制其中的值：</li>
				</ol>
				<ul class="text-muted-foreground mt-2 ml-4 list-inside list-disc space-y-1">
					<li><code>SESSDATA=</code> 后面的值复制到SESSDATA框</li>
					<li><code>bili_jct=</code> 后面的值复制到bili_jct框</li>
					<li><code>buvid3=</code> 后面的值复制到buvid3框</li>
					<li><code>DedeUserID=</code> 后面的值复制到DedeUserID框</li>
				</ul>
			</div>

			<div>
				<h4 class="font-medium">方法二：通过浏览器设置获取</h4>
				<ol class="text-muted-foreground mt-2 list-inside list-decimal space-y-1">
					<li>在Chrome中访问 chrome://settings/content/cookies</li>
					<li>搜索"bilibili.com"</li>
					<li>展开bilibili.com，找到对应的Cookie值</li>
				</ol>
			</div>

			<div class="bg-destructive/10 border-destructive/20 rounded-md border p-3">
				<h4 class="text-destructive font-medium">重要提醒</h4>
				<p class="text-destructive/80 mt-1">
					• 请确保在登录状态下获取Cookie<br />
					• 这些信息相当于您的登录凭证，请妥善保管<br />
					• 如果Cookie过期，需要重新获取
				</p>
			</div>
		</div>
		<AlertDialog.Footer>
			<AlertDialog.Action>我知道了</AlertDialog.Action>
		</AlertDialog.Footer>
	</AlertDialog.Content>
</AlertDialog.Root>
