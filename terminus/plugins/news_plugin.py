# /opt/ai-mcp/plugins/news_plugin.py
"""News tools migrated to plugin architecture."""
import sys
sys.path.insert(0, '/opt/ai-mcp')
from news_tools import register_news_tools

def register_plugin(mcp):
    register_news_tools(mcp)
