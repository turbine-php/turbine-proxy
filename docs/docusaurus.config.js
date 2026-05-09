// @ts-check
// `@type` JSDoc annotations allow editor autocompletion and type checking
// (when paired with `@ts-check`).
// There are various equivalent ways to declare your Docusaurus config.
// See: https://docusaurus.io/docs/api/docusaurus-config

import { themes as prismThemes } from 'prism-react-renderer';

/** @type {import('@docusaurus/types').Config} */
const config = {
  title: 'TurbineProxy',
  tagline: 'Intelligent MySQL & PostgreSQL proxy for product teams',
  favicon: 'img/favicon.svg',

  // Set the production url of your site here
  url: 'https://turbineproxy.dev',
  // Set the /<baseUrl>/ pathname under which your site is served
  baseUrl: '/',

  // GitHub pages deployment config (optional)
  organizationName: 'turbineproxy',
  projectName: 'turbineproxy',

  onBrokenLinks: 'throw',
  onBrokenMarkdownLinks: 'warn',

  // Even if you don't use internationalization, you can use this field to set
  // useful metadata like html lang. For example, if your site is Chinese, you
  // may want to replace "en" with "zh-Hans".
  i18n: {
    defaultLocale: 'en',
    locales: ['en', 'pt', 'es', 'fr', 'de', 'zh', 'pl'],
    localeConfigs: {
      en: { label: 'English', direction: 'ltr', htmlLang: 'en' },
      pt: { label: 'Português', direction: 'ltr', htmlLang: 'pt-BR' },
      es: { label: 'Español', direction: 'ltr', htmlLang: 'es' },
      fr: { label: 'Français', direction: 'ltr', htmlLang: 'fr' },
      de: { label: 'Deutsch', direction: 'ltr', htmlLang: 'de' },
      zh: { label: '中文', direction: 'ltr', htmlLang: 'zh-Hans' },
      pl: { label: 'Polski', direction: 'ltr', htmlLang: 'pl' },
    },
  },

  themes: [
    [
      require.resolve('@easyops-cn/docusaurus-search-local'),
      {
        hashed: true,
        language: ['en', 'pt', 'zh'],
        docsRouteBasePath: '/docs',
        highlightSearchTermsOnTargetPage: true,
      },
    ],
  ],

  presets: [
    [
      'classic',
      /** @type {import('@docusaurus/preset-classic').Options} */
      ({
        docs: {
          sidebarPath: './sidebars.js',
          editUrl: 'https://github.com/turbine-php/turbine-proxy/tree/main/docs/',
          showLastUpdateTime: true,
          showLastUpdateAuthor: false,
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      }),
    ],
  ],

  themeConfig:
    /** @type {import('@docusaurus/preset-classic').ThemeConfig} */
    ({
      image: 'img/social-card.png',
      colorMode: {
        defaultMode: 'dark',
        disableSwitch: false,
        respectPrefersColorScheme: true,
      },
      navbar: {
        title: 'TurbineProxy',
        logo: {
          alt: 'TurbineProxy Logo',
          src: 'img/logo.svg',
        },
        items: [
          {
            type: 'docSidebar',
            sidebarId: 'docsSidebar',
            position: 'left',
            label: 'Docs',
          },
          {
            href: '/docs/api/rest',
            label: 'API',
            position: 'left',
          },
          {
            type: 'localeDropdown',
            position: 'right',
          },
          {
            href: 'https://github.com/turbine-php/turbine-proxy',
            label: 'GitHub',
            position: 'right',
          },
        ],
      },
      footer: {
        style: 'dark',
        links: [
          {
            title: 'Docs',
            items: [
              { label: 'Getting Started', to: '/docs/getting-started/installation' },
              { label: 'Configuration', to: '/docs/configuration/reference' },
              { label: 'API Reference', to: '/docs/api/rest' },
            ],
          },
          {
            title: 'Features',
            items: [
              { label: 'Read/Write Splitting', to: '/docs/features/read-write-splitting' },
              { label: 'Query Analytics', to: '/docs/features/query-analytics' },
              { label: 'HA & Failover', to: '/docs/features/ha-failover' },
              { label: 'Dashboard', to: '/docs/features/dashboard' },
            ],
          },
          {
            title: 'Community',
            items: [
              { label: 'GitHub', href: 'https://github.com/turbine-php/turbine-proxy' },
            ],
          },
        ],
        copyright: `Copyright © ${new Date().getFullYear()} TurbineProxy. Built with Docusaurus.`,
      },
      prism: {
        theme: prismThemes.github,
        darkTheme: prismThemes.dracula,
        additionalLanguages: ['toml', 'bash', 'sql', 'json', 'rust'],
      },
    }),
};

export default config;
