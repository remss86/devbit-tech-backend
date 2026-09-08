// Run only against a disposable local test database, with NODE_ENV=test.
// Node 22+: node scripts/integration.mjs
import assert from 'node:assert/strict'
import { setTimeout as delay } from 'node:timers/promises'

const base = process.env.INTEGRATION_BASE_URL || 'http://127.0.0.1:3000/api'
const websocketUrl = process.env.INTEGRATION_WS_URL || 'ws://127.0.0.1:7878/api/ws'
assert.ok(['127.0.0.1', 'localhost'].includes(new URL(base).hostname), 'Local test server required')
assert.ok(
  ['127.0.0.1', 'localhost'].includes(new URL(websocketUrl).hostname),
  'Local WebSocket server required',
)
const run = Date.now()
let requestId = 0
let passed = 0
const sockets = []
async function request(path, method = 'GET', body, actor, expected = 200, ip) {
  // Independent simulated clients keep functional tests separate from the
  // rate-limit scenario below; the local reverse proxy trusts these headers.
  const headers = { 'X-Real-IP': ip || `198.18.${run % 250}.${++requestId}` }
  if (actor?.cookie) headers.Cookie = actor.cookie
  if (body && !(body instanceof FormData)) headers['Content-Type'] = 'application/json'
  const response = await fetch(base + path, {
    method,
    headers,
    body: body instanceof FormData ? body : body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(10000),
  })
  const text = await response.text()
  assert.equal(response.status, expected, `${method} ${path}: ${text}`)
  passed++
  console.log(`PASS ${method} ${path} (${expected})`)
  return {
    data: text && response.headers.get('content-type')?.includes('json') ? JSON.parse(text) : text,
    response,
  }
}
async function user(label) {
  const email = `${label}-${run}@example.test`
  const password = 'Integration123!'
  const name = `${label}-${run}`
  const code = (await request('/register/send_code', 'POST', { email })).data.developmentCode
  assert.ok(code, 'Server must expose development codes only in test mode')
  await request('/register', 'POST', { name, email, password, code })
  const login = await request('/login', 'POST', { email: email.toUpperCase(), password })
  assert.match(login.response.headers.get('set-cookie'), /HttpOnly/i)
  return {
    ...login.data,
    email,
    password,
    cookie: login.response.headers.get('set-cookie').split(';')[0],
  }
}
async function websocket(actor) {
  const socket = new WebSocket(websocketUrl)
  sockets.push(socket)
  const messages = []
  socket.addEventListener('message', (event) => messages.push(JSON.parse(event.data)))
  let connectionTimeout
  try {
    await new Promise((resolve, reject) => {
      connectionTimeout = setTimeout(() => reject(new Error('WebSocket connection timeout')), 10000)
      socket.addEventListener('open', resolve, { once: true })
      socket.addEventListener('error', reject, { once: true })
    })
  } finally {
    clearTimeout(connectionTimeout)
  }
  socket.send(JSON.stringify({ type: 'auth', token: actor.token }))
  const waitFor = async (predicate) => {
    for (let i = 0; i < 100; i++) {
      const message = messages.find(predicate)
      if (message) return message
      await delay(50)
    }
    throw new Error('WebSocket event timeout')
  }
  await waitFor((message) => message.type === 'auth_ok')
  return { socket, waitFor }
}
try {
  assert.equal((await request('/health')).data.db, 'ok')
  await request('/me', 'GET', undefined, undefined, 401)
  // The first two IDs are administrators in the current application.
  const reserved = await user('reserved')
  const alice = await user('Alice')
  const bob = await user('Bob')
  const outsider = await user('Outsider')
  assert.equal(bob.user.isAdmin, false)
  await request('/login', 'POST', { email: alice.email, password: 'incorrect' }, undefined, 401)
  assert.equal((await request('/me', 'GET', undefined, alice)).data.id, alice.user.id)
  const post = (
    await request(
      '/forum/posts',
      'POST',
      {
        title: `Integration ${run} 100%_`,
        content: '联调 **Markdown** $x^2$',
        category: 'general',
        tags: ['test'],
      },
      alice,
    )
  ).data
  const path = `/forum/posts/${post.id}`
  await request('/forum/posts', 'POST', { title: 'anonymous', content: 'no' }, undefined, 401)
  assert.equal((await request(path)).data.content, post.content)
  assert.ok(
    (await request('/forum/posts/search?q=100%25_')).data.some((item) => item.id === post.id),
  )
  assert.ok(
    (
      await request(
        `/forum/users/search?q=${encodeURIComponent(alice.user.name)}`,
        'GET',
        undefined,
        bob,
      )
    ).data.some((item) => item.id === alice.user.id),
  )
  await request(
    `/forum/posts/myposts/modify_post/${post.id}`,
    'PUT',
    { content: 'edited' },
    bob,
    403,
  )
  await request(`/forum/posts/myposts/modify_post/${post.id}`, 'PUT', { content: 'edited' }, alice)
  assert.equal((await request(path)).data.content, 'edited')
  assert.ok(
    (await request('/forum/posts/myposts', 'GET', undefined, alice)).data.some(
      (item) => item.id === post.id,
    ),
  )
  assert.equal((await request(path + '/like', 'PUT', undefined, bob)).data.likedByMe, true)
  assert.equal((await request(path, 'GET', undefined, outsider)).data.likedByMe, false)
  assert.equal((await request(path + '/like', 'PUT', undefined, bob)).data.likeCount, 0)
  if (reserved.user.isAdmin) {
    await request(path + '/like', 'PUT', undefined, reserved)
    assert.equal((await request(path + '/pin', 'PUT', undefined, reserved)).data.likedByMe, true)
    assert.equal((await request(path + '/lock', 'PUT', undefined, reserved)).data.likedByMe, true)
    await request(path + '/comments', 'POST', { content: 'locked' }, bob, 403)
    await request(path + '/lock', 'PUT', undefined, reserved)
  }
  const comment = (await request(path + '/comments', 'POST', { content: '评论😀' }, bob)).data
  assert.equal((await request(path)).data.commentCount, 1)
  await request(`/forum/comments/${comment.id}`, 'DELETE', undefined, outsider, 403)
  await request(`/forum/comments/${comment.id}`, 'DELETE', undefined, bob, 204)
  await request('/forum/friends', 'POST', { friendId: bob.user.id }, alice)
  assert.ok(
    (await request('/forum/friends', 'GET', undefined, alice)).data.some(
      (item) => item.user.id === bob.user.id,
    ),
  )
  const realtime = await websocket(bob)
  const message = (
    await request(
      '/forum/messages',
      'POST',
      { recipientId: bob.user.id, content: '实时消息😀'.repeat(20) },
      alice,
    )
  ).data
  assert.equal(
    (await realtime.waitFor((item) => item.type === 'new_message')).message_id,
    message.id,
  )
  console.log('PASS WebSocket authentication and Unicode notification')
  assert.ok(
    (await request('/forum/messages', 'GET', undefined, bob)).data.some(
      (item) => item.id === message.id,
    ),
  )
  assert.equal((await request('/forum/messages', 'GET', undefined, outsider)).data.length, 0)
  assert.equal((await request('/forum/bootstrap')).data.messages.length, 0)
  await request(`/forum/messages/${message.id}/read`, 'PUT', undefined, outsider, 404)
  await request(`/forum/messages/${message.id}/read`, 'PUT', undefined, bob, 204)
  // Opening an already-read/empty conversation must also succeed.
  await request(`/forum/messages/conversation/${alice.user.id}/read`, 'PUT', undefined, bob, 204)
  await request('/forum/messages/conversation/2147483647/read', 'PUT', undefined, bob, 404)
  const form = new FormData()
  form.set(
    'avatar',
    new Blob(
      [
        Buffer.from(
          'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aD1sAAAAASUVORK5CYII=',
          'base64',
        ),
      ],
      { type: 'image/png' },
    ),
    'avatar.png',
  )
  const avatar = (await request('/me/avatar', 'POST', form, bob)).data.avatarUrl
  await request(avatar.replace(/^\/api/, ''))
  await request(`/forum/friends/${bob.user.id}`, 'DELETE', undefined, alice, 204)
  await request(path, 'DELETE', undefined, outsider, 403)
  await request(path, 'DELETE', undefined, alice, 204)
  await request(path, 'GET', undefined, undefined, 404)
  await request('/logout', 'POST', undefined, bob)
  for (let i = 0; i < 50 && realtime.socket.readyState !== WebSocket.CLOSED; i++) await delay(50)
  assert.equal(realtime.socket.readyState, WebSocket.CLOSED)
  const authIp = `198.19.${run % 250}.1`
  const generalIp = `198.19.${run % 250}.2`
  for (let i = 0; i < 5; i++)
    await request(
      '/login',
      'POST',
      { email: 'absent@example.test', password: 'wrong' },
      undefined,
      401,
      authIp,
    )
  const limited = await request(
    '/login',
    'POST',
    { email: 'absent@example.test', password: 'wrong' },
    undefined,
    429,
    authIp,
  )
  assert.ok(Number(limited.response.headers.get('retry-after')) > 0)
  for (let i = 0; i < 120; i++)
    await request('/forum/posts', 'GET', undefined, undefined, 200, generalIp)
  await request('/forum/posts', 'GET', undefined, undefined, 429, generalIp)
  console.log(`PASS: ${passed} HTTP assertions plus WebSocket assertions`)
} finally {
  for (const socket of sockets) socket.close()
}
