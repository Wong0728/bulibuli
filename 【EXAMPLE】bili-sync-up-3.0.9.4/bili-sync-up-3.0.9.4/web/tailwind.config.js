/** @type {import('tailwindcss').Config} */
export default {
	content: ['./src/**/*.{html,js,svelte,ts}'],
	theme: {
		extend: {
			screens: {
				xs: '475px',
				sm: '640px',
				md: '768px',
				lg: '1024px',
				xl: '1280px',
				'2xl': '1536px'
			},
			spacing: {
				18: '4.5rem',
				88: '22rem'
			},
			gridTemplateColumns: {
				'auto-fit-sm': 'repeat(auto-fit, minmax(280px, 1fr))',
				'auto-fit-md': 'repeat(auto-fit, minmax(300px, 1fr))',
				'auto-fit-lg': 'repeat(auto-fit, minmax(320px, 1fr))'
			}
		}
	},
	plugins: []
};
