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
        'features/connection-pooling',
        'features/query-analytics',
        'features/query-routing',
        'features/query-rewriting',
        'features/ha-failover',
        'features/sql-injection-protection',
        'features/dashboard',
        'features/grafana-integration',
        'features/cluster-sync',
        'features/audit-log',
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
  ],
};

export default sidebars;
