import React from 'react'
import ReactDOM from 'react-dom/client'
import { HashRouter, Routes, Route, useLocation } from 'react-router'
import { useEffect } from 'react'
import './index.css'
import Layout from './components/Layout'
import DocsHome from './pages/DocsHome'
import DocPage from './pages/DocPage'

// Import all doc content, per version (10.1 current at /docs, 9.x at /v9/docs)
import { versions } from './content/versions'

function ScrollToTop() {
  const { pathname } = useLocation()
  useEffect(() => { window.scrollTo(0, 0) }, [pathname])
  return null
}

ReactDOM.createRoot(document.getElementById('root')).render(
  <React.StrictMode>
    <HashRouter>
      <ScrollToTop />
      <Routes>
        <Route element={<Layout />}>
          {/* Each version's docs index; the marketing landing is the site root, outside this app */}
          <Route path="/" element={<DocsHome version="current" />} />
          <Route path="/v9" element={<DocsHome version="v9" />} />
          {Object.values(versions).flatMap(version => Object.entries(version.pages).map(([path, page]) => (
            <Route key={path} path={path} element={<DocPage {...page} version={version.id} />} />
          )))}
        </Route>
      </Routes>
    </HashRouter>
  </React.StrictMode>
)
