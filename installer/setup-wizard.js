#!/usr/bin/env node

// This is the new Docker-based setup wizard
// It runs everything in Docker to avoid Python environment issues
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

// Simply forward to the Docker-based wizard
import(join(__dirname, 'setup-wizard-docker.js'));