import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { initApiConfig } from './lib/api';
import './styles.css';

// Where the API lives is resolved before the first render, so every accessor
// below can stay synchronous. A failure here is not fatal: the defaults still
// point at loopback, and the app has an offline state with a retry.
initApiConfig().finally(() => {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
});
