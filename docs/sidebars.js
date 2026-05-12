/** @type {import('@docusaurus/plugin-content-docs').SidebarsConfig} */
const sidebars = {
  docsSidebar: [
    {
      type: 'doc',
      id: 'intro',
      label: 'Introduction',
    },
    {
      type: 'category',
      label: 'Getting Started',
      collapsed: false,
      items: [
        'getting-started/installation',
        'getting-started/quick-start',
        'getting-started/configuration',
      ],
    },
    {
      type: 'category',
      label: 'Features',
      items: [
        'features/read-write-splitting',
        'features/gtid-ryow',
        'features/connection-pooling',
        'features/query-analytics',
        'features/query-routing',
        'features/query-rewriting',
        'features/ha-failover',
        'features/sql-injection-protection',
        'features/compression',
        'features/fast-forward',
        'features/ssl-keylog',
        'features/dashboard',
        'features/grafana-integration',
        'features/cluster-sync',
        'features/audit-log',
        'features/failure-modes',
      ],
    },
    {
      type: 'category',
      label: 'Configuration Reference',
      items: [
        'configuration/reference',
        'configuration/backends',
        'configuration/users',
        'configuration/routing-rules',
        'configuration/rewrite-rules',
        'configuration/analytics',
        'configuration/dashboard',
        'configuration/ha',
        'configuration/tls',
        'configuration/security',
      ],
    },
    {
      type: 'category',
      label: 'Dashboard Guide',
      items: [
        'dashboard/overview',
        'dashboard/panels',
        'dashboard/cluster',
      ],
    },
    {
      type: 'category',
      label: 'API Reference',
      items: [
        'api/rest',
        'api/grafana',
        'api/mcp',
      ],
    },
    {
      type: 'category',
      label: 'Deployment',
      items: [
        'deployment/production',
        'deployment/docker',
        'deployment/ha-setup',
      ],
    },
    {
      type: 'category',
      label: 'Tutorials',
      collapsed: false,
      items: [
        {
          type: 'category',
          label: '🟢 Beginner',
          items: [
            'tutorials/what-is-a-database-proxy',
            'tutorials/install-and-run',
            'tutorials/connect-your-app',
            'tutorials/explore-the-dashboard',
          ],
        },
        {
          type: 'category',
          label: '🟡 Intermediate',
          items: [
            'tutorials/read-write-splitting',
            'tutorials/connection-pooling-tuning',
            'tutorials/query-rewriting',
            'tutorials/rate-limiting-and-routing',
          ],
        },
        {
          type: 'category',
          label: '🔴 Advanced',
          items: [
            'tutorials/high-availability',
            'tutorials/secrets-encryption',
            'tutorials/prometheus-grafana',
            'tutorials/kubernetes-helm',
          ],
        },
        {
          type: 'category',
          label: '⚡ How-To',
          items: [
            'tutorials/howto-sql-injection-protection',
            'tutorials/howto-query-rules-dry-run',
            'tutorials/howto-hot-reload',
            'tutorials/howto-migrate-from-proxysql',
            'tutorials/howto-stored-procedures',
          ],
        },
      ],
    },
  ],
};

export default sidebars;
