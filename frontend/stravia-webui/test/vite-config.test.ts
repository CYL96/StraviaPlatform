import { afterEach, describe, expect, test } from 'bun:test'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { createServer, type ViteDevServer } from 'vite'
import config from '../vite.config'

const originalPort = process.env.STRAVIA_PORT

afterEach(() => {
  if (originalPort === undefined) {
    delete process.env.STRAVIA_PORT
  } else {
    process.env.STRAVIA_PORT = originalPort
  }
})

describe('Vite development proxy', () => {
  test('prefers the current STRAVIA_PORT over the repository .env value', () => {
    process.env.STRAVIA_PORT = '45678'

    expect(typeof config).toBe('function')
    if (typeof config !== 'function') {
      throw new Error('expected a mode-aware Vite config')
    }

    const resolved = config({ command: 'serve', mode: 'development', isSsrBuild: false, isPreview: false })
    const proxies = ['/api/v1', '/v1', '/v1beta'].map((path) => resolved.server?.proxy?.[path])

    for (const proxy of proxies) {
      expect(proxy).toBeObject()
      if (!proxy || typeof proxy === 'string') {
        throw new Error('expected an object proxy configuration')
      }
      expect(proxy.target).toBe('http://127.0.0.1:45678')
    }
  })

  test('keeps concurrent development frontends on separate reachable ports', async () => {
    if (typeof config !== 'function') {
      throw new Error('expected a mode-aware Vite config')
    }
    const development = config({ command: 'serve', mode: 'development', isSsrBuild: false, isPreview: false })
    const directory = await mkdtemp(join(tmpdir(), 'stravia-vite-workspaces-'))
    const servers: ViteDevServer[] = []

    async function start(workspace: string, port: number): Promise<URL> {
      const root = join(directory, workspace)
      await mkdir(root)
      await writeFile(join(root, 'index.html'), `<h1>${workspace}</h1>`)
      const server = await createServer({
        configFile: false,
        root,
        logLevel: 'silent',
        server: { ...development.server, port },
      })
      servers.push(server)
      await server.listen()
      const origin = server.resolvedUrls?.local[0]
      if (!origin) throw new Error('development frontend has no local URL')
      return new URL(origin)
    }

    try {
      const first = await start('first-workspace', 0)
      const second = await start('second-workspace', Number(first.port))

      expect(second.port).not.toBe(first.port)
      expect(await (await fetch(first)).text()).toContain('<h1>first-workspace</h1>')
      expect(await (await fetch(second)).text()).toContain('<h1>second-workspace</h1>')
    } finally {
      for (const server of servers.reverse()) await server.close()
      await rm(directory, { recursive: true, force: true })
    }
  })
})
