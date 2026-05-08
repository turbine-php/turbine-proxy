import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { I18nProvider } from '@lingui/react'
import { i18n, initI18n } from './i18n.js'
import './index.css'
import App from './App.jsx'

initI18n().then(() => {
  createRoot(document.getElementById('root')).render(
    <StrictMode>
      <I18nProvider i18n={i18n}>
        <App />
      </I18nProvider>
    </StrictMode>,
  )
})
