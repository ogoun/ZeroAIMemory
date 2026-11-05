// Обновление токена доступа в одной вкладке витрины (фикстура карты кода ТС-2, задача 4 M6).

const CHANNEL = "storefront-auth";

export interface TokenPair {
  access: string;
  expiresAt: number;
}

/** Обновляет токен в одной вкладке и рассылает его остальным через BroadcastChannel. */
export class SingleTabRefresher {
  private channel = new BroadcastChannel(CHANNEL);

  constructor(private readonly refresh: () => Promise<TokenPair>) {}

  async refreshNow(): Promise<TokenPair> {
    const pair = await this.refresh();
    this.channel.postMessage(pair);
    return pair;
  }

  onToken(cb: (pair: TokenPair) => void): void {
    this.channel.onmessage = (e) => cb(e.data as TokenPair);
  }
}
