#!/usr/bin/env node

/**
 * Post release announcement to GitHub Discussions
 * Requires GITHUB_TOKEN environment variable with discussion:write permission
 */

import https from 'https';

const GITHUB_API_URL = 'api.github.com';
const OWNER = 'ramakay';
const REPO = 'claude-self-reflect';
const CATEGORY = 'Announcements'; // Update if your category is different

async function getRepositoryId() {
  const query = `
    query {
      repository(owner: "${OWNER}", name: "${REPO}") {
        id
        discussionCategories(first: 10) {
          nodes {
            id
            name
          }
        }
      }
    }
  `;

  return makeGraphQLRequest(query);
}

async function createDiscussion(repositoryId, categoryId, title, body) {
  const mutation = `
    mutation {
      createDiscussion(input: {
        repositoryId: "${repositoryId}"
        categoryId: "${categoryId}"
        title: "${title}"
        body: "${body.replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n')}"
      }) {
        discussion {
          id
          url
        }
      }
    }
  `;

  return makeGraphQLRequest(mutation);
}

function makeGraphQLRequest(query) {
  return new Promise((resolve, reject) => {
    const token = process.env.GITHUB_TOKEN;
    if (!token) {
      reject(new Error('GITHUB_TOKEN environment variable is required'));
      return;
    }

    const options = {
      hostname: GITHUB_API_URL,
      path: '/graphql',
      method: 'POST',
      headers: {
        'Authorization': `Bearer ${token}`,
        'Content-Type': 'application/json',
        'User-Agent': 'claude-self-reflect-announcer'
      }
    };

    const req = https.request(options, (res) => {
      let data = '';
      res.on('data', (chunk) => data += chunk);
      res.on('end', () => {
        try {
          const result = JSON.parse(data);
          if (result.errors) {
            reject(new Error(`GraphQL Error: ${JSON.stringify(result.errors)}`));
          } else {
            resolve(result.data);
          }
        } catch (e) {
          reject(e);
        }
      });
    });

    req.on('error', reject);
    req.write(JSON.stringify({ query }));
    req.end();
  });
}

async function main() {
  try {
    console.log('📢 Posting v3.0 Release Announcement to GitHub Discussions...\n');

    // Get repository and category IDs
    console.log('Fetching repository information...');
    const repoData = await getRepositoryId();
    const repositoryId = repoData.repository.id;
    const announcementCategory = repoData.repository.discussionCategories.nodes.find(
      cat => cat.name === CATEGORY
    );

    if (!announcementCategory) {
      throw new Error(`Category "${CATEGORY}" not found. Available categories: ${
        repoData.repository.discussionCategories.nodes.map(c => c.name).join(', ')
      }`);
    }

    const title = "🚀 Claude Self-Reflect v3.0.0 Released - Major Architecture Overhaul";
    
    // Keep body concise to avoid timeout (< 1000 chars as per reflection)
    const body = `## Claude Self-Reflect v3.0.0 is now available!

This major release brings a complete architectural overhaul with modular design and critical fixes.

### 🎯 Key Highlights

**Modular Architecture**
- Transformed monolithic 570-line script into 15+ focused modules
- Clean separation of concerns with dependency injection
- SOLID principles throughout

**Token-Aware Batching**
- Fixes issue #38: Voyage AI 120k token limit errors
- Intelligent batch splitting with token estimation
- Automatic text truncation with debug logging

**Enhanced Performance**
- 50% reduction in memory usage during imports
- Improved error handling and recovery
- Conditional imports for optional dependencies

### 📦 Installation

\`\`\`bash
npm install -g claude-self-reflect@3.0.0
claude-self-reflect setup
\`\`\`

### 📚 Resources

- [Release Notes](https://github.com/${OWNER}/${REPO}/releases/tag/v3.0.0)
- [Migration Guide](https://github.com/${OWNER}/${REPO}/blob/main/docs/operations/RELEASE_NOTES_v3.0.md#migration-guide)
- [Documentation](https://github.com/${OWNER}/${REPO}#readme)

### 💬 Feedback

Please share your experience with v3.0 in the comments below!`;

    // Create the discussion
    console.log('Creating discussion...');
    const discussion = await createDiscussion(
      repositoryId,
      announcementCategory.id,
      title,
      body
    );

    console.log('\n✅ Announcement posted successfully!');
    console.log(`📍 URL: ${discussion.createDiscussion.discussion.url}`);
    console.log(`🆔 ID: ${discussion.createDiscussion.discussion.id}`);

  } catch (error) {
    console.error('❌ Error posting announcement:', error.message);
    if (error.message.includes('504') || error.message.includes('timeout')) {
      console.log('\n⚠️  Note: If you got a timeout error, the discussion may have been created anyway.');
      console.log('Check: https://github.com/' + OWNER + '/' + REPO + '/discussions');
    }
    process.exit(1);
  }
}

// Run if called directly
main();