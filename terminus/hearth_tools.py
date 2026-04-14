import subprocess, json, os

# ============================================================
# Hearth Tools — Kitchen Management via Grocy
# CT214 SSHes to CT310, calls Grocy at 172.17.0.1:9283
# ============================================================

HEARTH_HOST = 'root@YOUR_FLEET_SERVER_IP'
GROCY_URL = os.environ.get('GROCY_URL', 'http://172.17.0.1:9283')
GROCY_KEY = os.environ.get('GROCY_API_KEY', '')

def _grocy(endpoint, method='GET', data=None, timeout=30):
    auth = f'-H "GROCY-API-KEY: {GROCY_KEY}"' if GROCY_KEY else ''
    if data:
        d_str = json.dumps(data).replace("'", '"')
        cmd = f"curl -s -X {method} {auth} -H 'Content-Type: application/json' -d '{d_str}' {GROCY_URL}/api{endpoint}"
    else:
        cmd = f"curl -s -X {method} {auth} {GROCY_URL}/api{endpoint}"
    full = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {HEARTH_HOST} '{cmd}'"
    result = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=timeout)
    if not result.stdout.strip():
        return {'error': result.stderr[:200] or 'No response'}
    try:
        return json.loads(result.stdout)
    except:
        return {'raw': result.stdout[:300]}

def register_hearth_tools(mcp):

    @mcp.tool()
    def hearth_pantry(query: str = '') -> dict:
        """List pantry inventory. query: optional search term for specific item."""
        if query:
            return _grocy(f'/stock?query%5B%5D=name%25%25{query}')
        return _grocy('/stock')

    @mcp.tool()
    def hearth_pantry_add(product_name: str, quantity: float = 1, unit: str = '', best_before: str = '') -> dict:
        """Add an item to pantry inventory. best_before: YYYY-MM-DD format."""
        # First find or create the product
        products = _grocy(f'/objects/products?query%5B%5D=name%25%25{product_name}')
        if isinstance(products, list) and products:
            product_id = products[0]['id']
        else:
            # Create product
            new_product = _grocy('/objects/products', 'POST', {'name': product_name, 'description': ''})
            product_id = new_product.get('created_object_id', '')
        if not product_id:
            return {'error': f'Could not find or create product: {product_name}'}
        data = {'amount': quantity, 'transaction_type': 'purchase'}
        if best_before:
            data['best_before_date'] = best_before
        return _grocy(f'/stock/products/{product_id}/add', 'POST', data)

    @mcp.tool()
    def hearth_expiring(days: int = 7) -> dict:
        """List items expiring within N days."""
        return _grocy(f'/stock/volatile?due_soon_days={days}')

    @mcp.tool()
    def hearth_shopping_list() -> dict:
        """Get current shopping list."""
        return _grocy('/shopping_list')

    @mcp.tool()
    def hearth_shopping_add(items: str) -> dict:
        """Add items to shopping list. items: comma-separated item names."""
        results = []
        for item in items.split(','):
            item = item.strip()
            if item:
                result = _grocy('/shopping_list/add-item', 'POST', {'name': item})
                results.append({'item': item, 'result': result})
        return {'added': results}

    @mcp.tool()
    def hearth_what_can_i_make() -> dict:
        """Check what recipes can be made with current pantry inventory."""
        return _grocy('/recipes?include_not_fulfilled=false')

    @mcp.tool()
    def hearth_meal_plan(week: str = '') -> dict:
        """View current meal plan. week: YYYY-WXX format or empty for current week."""
        return _grocy('/meal_plan')
