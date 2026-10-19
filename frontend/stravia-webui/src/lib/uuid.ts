/** 用于本地记录和 DOM 标识；HTTP 管理入口也必须保留加密随机性。 */
export function createUuid(): string {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  // randomUUID 仅在安全上下文可用，getRandomValues 没有此限制。
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  bytes[6] = (bytes[6] & 0x0f) | 0x40
  bytes[8] = (bytes[8] & 0x3f) | 0x80
  let uuid = ''
  for (let index = 0; index < bytes.length; index++) {
    if (index === 4 || index === 6 || index === 8 || index === 10) uuid += '-'
    uuid += bytes[index].toString(16).padStart(2, '0')
  }
  return uuid
}
