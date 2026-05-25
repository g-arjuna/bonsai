import adapter from '@sveltejs/adapter-static';

/** @type {import('@sveltejs/kit').Config} */
const config = {
    kit: {
        adapter: adapter({
            pages: 'dist-bonpy',
            assets: 'dist-bonpy',
            fallback: 'index.html',
            precompress: false,
        }),
        paths: {
            base: '/bonpy',
        },
        alias: {
            $lib: './src/lib',
        },
    },
};

export default config;
