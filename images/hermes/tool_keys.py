"""FAL v0.13.1's process-global client cannot read Hermes' profile scope.

Resolve the placeholder for each native image request and bind it to that
request's SDK client/handle. Never write os.environ or cache across profiles.
Delete when upstream's direct FAL path uses a scoped explicit-key client.
"""

from agent.secret_scope import get_secret
from tools import image_generation_tool as images
from tools.tool_backend_helpers import read_selection


class ScopedHandle:
    def __init__(self, client, handle):
        self.client = client
        self.handle = handle

    def get(self):
        try:
            return self.handle.get()
        finally:
            self.client._client.close()


def install():
    original = images._submit_fal_request

    def submit(model, arguments):
        if read_selection('image_gen') != 'fal':
            return original(model, arguments)
        key = get_secret('FAL_KEY', '')
        if not key:
            raise ValueError('FAL_KEY is not set for this profile')
        import uuid
        from fal_client import SyncClient
        client = SyncClient(key=key)
        try:
            handle = client.submit(model, arguments=arguments,
                                   headers={'x-idempotency-key': str(uuid.uuid4())})
        except Exception:
            client._client.close()
            raise
        return ScopedHandle(client, handle)

    images._submit_fal_request = submit
