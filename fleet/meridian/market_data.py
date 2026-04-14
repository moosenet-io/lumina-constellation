"""Market data fetchers for Meridian (paper trading). SIMULATED — read-only market data."""
import json, urllib.request, urllib.parse, os, time
from datetime import datetime

ALPHAVANTAGE_KEY = os.environ.get('ALPHAVANTAGE_API_KEY', '')
FINNHUB_KEY = os.environ.get('FINNHUB_API_KEY', '')
_cache = {}
_CACHE_TTL = 300  # 5 min

def _cached(key, fetch_fn, ttl=_CACHE_TTL):
    if key in _cache and time.time() < _cache[key][1]:
        return _cache[key][0]
    data = fetch_fn()
    _cache[key] = (data, time.time() + ttl)
    return data

def get_crypto_prices(symbols=None):
    """CoinGecko — no key needed. Returns {BTC: {price, change_24h, market_cap}}"""
    if symbols is None:
        symbols = ['BTC', 'ETH', 'SOL', 'BNB', 'AVAX']
    id_map = {'BTC':'bitcoin','ETH':'ethereum','SOL':'solana','BNB':'binancecoin','AVAX':'avalanche-2','DOT':'polkadot','MATIC':'matic-network'}
    ids = ','.join(id_map.get(s,s.lower()) for s in symbols)
    def fetch():
        url = f'https://api.coingecko.com/api/v3/simple/price?ids={ids}&vs_currencies=usd&include_24hr_change=true&include_market_cap=true'
        try:
            req = urllib.request.Request(url, headers={'User-Agent': 'Meridian/1.0 (SIMULATED)'})
            with urllib.request.urlopen(req, timeout=10) as r:
                raw = json.load(r)
            result = {}
            for sym, cg_id in id_map.items():
                if cg_id in raw:
                    result[sym] = {'price': raw[cg_id].get('usd', 0), 'change_24h': raw[cg_id].get('usd_24h_change', 0), 'market_cap': raw[cg_id].get('usd_market_cap', 0)}
            return result
        except Exception as e:
            return {'error': str(e)}
    return _cached('crypto', fetch, 300)

def get_fear_greed():
    """Alternative.me Fear & Greed Index — free, no key."""
    def fetch():
        try:
            req = urllib.request.Request('https://api.alternative.me/fng/?limit=1', headers={'User-Agent': 'Meridian/1.0'})
            with urllib.request.urlopen(req, timeout=10) as r:
                d = json.load(r)
            data = d.get('data', [{}])[0]
            return {'value': int(data.get('value', 50)), 'label': data.get('value_classification', 'Neutral'), 'timestamp': data.get('timestamp', '')}
        except Exception as e:
            return {'value': 50, 'label': 'Unknown', 'error': str(e)}
    return _cached('fear_greed', fetch, 3600)

def get_technical_indicators(symbol='BTC', indicator='RSI', period=14):
    """Alpha Vantage technical indicators. ALPHAVANTAGE_API_KEY required."""
    if not ALPHAVANTAGE_KEY:
        return {'error': 'ALPHAVANTAGE_API_KEY not set'}
    av_symbol = {'BTC': 'BTC', 'ETH': 'ETH', 'SOL': 'SOL'}.get(symbol, symbol)
    def fetch():
        try:
            params = {'function': f'CRYPTO_{indicator}', 'symbol': av_symbol, 'market': 'USD', 'time_period': period, 'interval': 'daily', 'apikey': ALPHAVANTAGE_KEY}
            url = 'https://www.alphavantage.co/query?' + urllib.parse.urlencode(params)
            req = urllib.request.Request(url, headers={'User-Agent': 'Meridian/1.0'})
            with urllib.request.urlopen(req, timeout=15) as r:
                d = json.load(r)
            # Get latest value
            key = [k for k in d if 'Technical Analysis' in k]
            if key:
                latest = list(d[key[0]].values())[0]
                return {'indicator': indicator, 'value': float(list(latest.values())[0]), 'symbol': symbol}
            return {'indicator': indicator, 'value': None, 'raw': str(d)[:100]}
        except Exception as e:
            return {'indicator': indicator, 'error': str(e)}
    return _cached(f'{indicator}_{symbol}', fetch, 3600)  # cache 1h (25 calls/day limit)

def get_stock_quote(ticker='SPY'):
    """Finnhub stock quote. FINNHUB_API_KEY required."""
    if not FINNHUB_KEY:
        return {'error': 'FINNHUB_API_KEY not set'}
    def fetch():
        try:
            url = f'https://finnhub.io/api/v1/quote?symbol={ticker}&token={FINNHUB_KEY}'
            req = urllib.request.Request(url, headers={'User-Agent': 'Meridian/1.0'})
            with urllib.request.urlopen(req, timeout=10) as r:
                d = json.load(r)
            return {'ticker': ticker, 'price': d.get('c', 0), 'change': d.get('d', 0), 'change_pct': d.get('dp', 0), 'high': d.get('h', 0), 'low': d.get('l', 0)}
        except Exception as e:
            return {'ticker': ticker, 'error': str(e)}
    return _cached(f'stock_{ticker}', fetch, 60)

def get_market_sentiment(query='bitcoin price today'):
    """SearXNG news search for market sentiment. Internal, no key needed."""
    try:
        url = f'http://YOUR_SEARXNG_IP:8088/search?{urllib.parse.urlencode({"q": query, "format": "json", "categories": "news"})}'
        req = urllib.request.Request(url, headers={'User-Agent': 'Meridian/1.0'})
        with urllib.request.urlopen(req, timeout=8) as r:
            d = json.load(r)
        headlines = [{'title': r.get('title','')[:80], 'url': r.get('url','')} for r in d.get('results',[])[:5]]
        return {'headlines': headlines, 'count': len(headlines), 'query': query}
    except Exception as e:
        return {'headlines': [], 'error': str(e)}

if __name__ == '__main__':
    import sys
    # Quick test of all data sources
    print('=== Meridian Market Data Test (SIMULATED) ===')
    print('Crypto:', get_crypto_prices(['BTC', 'ETH']))
    print('Fear/Greed:', get_fear_greed())
    print('RSI:', get_technical_indicators('BTC', 'RSI'))
    print('SPY:', get_stock_quote('SPY'))
    print('Sentiment:', get_market_sentiment('bitcoin'))
