import subprocess, json, os

# ============================================================
# Relay Tools — Vehicle Maintenance via LubeLogger
# terminus-host SSHes to fleet-host, calls LubeLogger at localhost:5000
# LubeLogger has EnableAuth=false, no token needed
# ============================================================

RELAY_HOST = 'root@YOUR_FLEET_SERVER_IP'
LUBELOGGER_URL = os.environ.get('LUBELOGGER_URL', '')

def _lubelogger(endpoint, method='GET', data=None, timeout=30):
    if not LUBELOGGER_URL:
        return {'error': 'LUBELOGGER_URL not configured'}
    if data:
        d_str = json.dumps(data).replace("'", '"')
        cmd = f"curl -s -X {method} -H 'Content-Type: application/json' -d '{d_str}' {LUBELOGGER_URL}{endpoint}"
    else:
        cmd = f"curl -s -X {method} {LUBELOGGER_URL}{endpoint}"
    full = f"ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no {RELAY_HOST} '{cmd}'"
    result = subprocess.run(full, shell=True, capture_output=True, text=True, timeout=timeout)
    if not result.stdout.strip():
        return {'error': result.stderr[:200] or 'No response'}
    try:
        return json.loads(result.stdout)
    except:
        return {'raw': result.stdout[:300]}

def register_relay_tools(mcp):

    @mcp.tool()
    def relay_vehicles() -> dict:
        """List all vehicles in LubeLogger with IDs and details."""
        return _lubelogger('/api/Vehicles')

    @mcp.tool()
    def relay_service_log(vehicle_id: str, service_type: str, mileage: int, cost: float, shop: str = '', notes: str = '') -> dict:
        """Log a vehicle service visit to LubeLogger.
        vehicle_id: from relay_vehicles(). service_type: Oil Change, Tire Rotation, etc."""
        from datetime import date
        data = {'date': date.today().isoformat(), 'mileage': mileage, 'description': service_type,
                'cost': cost, 'vendor': shop, 'notes': notes}
        return _lubelogger(f'/api/Vehicle/{vehicle_id}/servicerecord', 'POST', data)

    @mcp.tool()
    def relay_fuel_log(vehicle_id: str, gallons: float, cost: float, mileage: int, full_tank: bool = True) -> dict:
        """Log a fuel fill-up to LubeLogger."""
        from datetime import date
        data = {'date': date.today().isoformat(), 'mileage': mileage,
                'gallons': gallons, 'cost': cost, 'isFillToFull': full_tank}
        return _lubelogger(f'/api/Vehicle/{vehicle_id}/gasrecord', 'POST', data)

    @mcp.tool()
    def relay_next_due(vehicle_id: str = '') -> dict:
        """Get upcoming maintenance reminders. vehicle_id: optional, empty = all vehicles."""
        if vehicle_id:
            return _lubelogger(f'/api/Vehicle/{vehicle_id}/reminders')
        vehicles = _lubelogger('/api/Vehicles')
        if isinstance(vehicles, list):
            return {'reminders': [{'vehicle': v.get('year','') + ' ' + v.get('make','') + ' ' + v.get('model',''),
                                   'reminders': _lubelogger(f'/api/Vehicle/{v["id"]}/reminders')} for v in vehicles[:5]]}
        return vehicles

    @mcp.tool()
    def relay_service_history(vehicle_id: str) -> dict:
        """Get full service history for a vehicle."""
        return _lubelogger(f'/api/Vehicle/{vehicle_id}/servicerecord')

    @mcp.tool()
    def relay_mileage_update(vehicle_id: str, mileage: int) -> dict:
        """Update current odometer reading for a vehicle."""
        from datetime import date
        return _lubelogger(f'/api/Vehicle/{vehicle_id}/odometerrecord', 'POST',
                          {'date': date.today().isoformat(), 'mileage': mileage})

    @mcp.tool()
    def relay_cost_summary(vehicle_id: str) -> dict:
        """Get cost analysis and total spending for a vehicle."""
        return _lubelogger(f'/api/Vehicle/{vehicle_id}/costanalysis')
